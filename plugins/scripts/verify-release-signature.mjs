#!/usr/bin/env node

/**
 * Release 产物签名验证脚本（GitHub Actions 打包签名后、上传前运行）。
 *
 * 目的：拦截「release 成功但 SPARK_PLUGIN_SIGNING_PRIVATE_KEY 配错」的隐蔽故障——
 * build-plugin-package.mjs 用任意有效 PEM 私钥都能成功签名，但客户端只信任
 * app/src-tauri/src/market/trust.rs 的内置公钥；私钥不配对则产物验签必挂。
 *
 * 两道检查（任一失败 → 非 0 退出，workflow 失败、不发布）：
 *   1. 产物公钥 update-manifest.pub.pem（由签名私钥派生）与内置信任公钥逐字节一致
 *      —— 私钥派生的公钥与内置公钥一致 ⇔ secret 与本机私钥是同一把；
 *   2. 用内置公钥对 update-manifest.json + update-manifest.sig 做 Ed25519 验签
 *      —— 等价于客户端 verify_manifest_signature 的全链路复现。
 *
 * 用法：
 *   node plugins/scripts/verify-release-signature.mjs \
 *     --manifest app/dist-market/plugins/<pluginId>/update-manifest.json \
 *     --sig      app/dist-market/plugins/<pluginId>/update-manifest.sig \
 *     --pub      app/dist-market/plugins/<pluginId>/update-manifest.pub.pem
 *   （trust.rs 路径固定在 code/app/src-tauri/src/market/trust.rs，无需传参）
 */

import { createPublicKey, verify } from 'crypto';
import { readFileSync } from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const codeRoot = path.resolve(__dirname, '../..');
const TRUST_RS = path.join(codeRoot, 'app', 'src-tauri', 'src', 'market', 'trust.rs');

function parseArgs(argv) {
  const args = {};
  for (let i = 2; i < argv.length; i += 1) {
    if (!argv[i].startsWith('--')) {
      continue;
    }
    args[argv[i].slice(2)] = argv[i + 1];
    i += 1;
  }
  return args;
}

/** 从 trust.rs 提取 DEFAULT_PLUGIN_PUBLIC_KEYS_PEM 数组首项（处理 \n 字面转义）。 */
function readBuiltinPublicKeyPem() {
  const src = readFileSync(TRUST_RS, 'utf8');
  // 注意 const 声明中间有 `: [&str; 1] = [`，不能用 [^[]* 跳过（会被 [&str; 1] 的 [ 截断）
  const match = src.match(/DEFAULT_PLUGIN_PUBLIC_KEYS_PEM[\s\S]*?\[\s*"([^"]+)"/);
  if (!match) {
    throw new Error(`无法从 ${TRUST_RS} 提取 DEFAULT_PLUGIN_PUBLIC_KEYS_PEM`);
  }
  return match[1].replace(/\\n/g, '\n').trim();
}

/** PEM → body base64（去掉 ----- 头尾行）。 */
function pemBody(pem) {
  return pem
    .split('\n')
    .filter((line) => !line.trim().startsWith('-----'))
    .join('')
    .trim();
}

function main() {
  const args = parseArgs(process.argv);
  const manifestPath = args.manifest;
  const sigPath = args.sig;
  const pubPath = args.pub;

  if (!manifestPath || !sigPath || !pubPath) {
    throw new Error(
      '用法：node verify-release-signature.mjs --manifest <update-manifest.json> --sig <update-manifest.sig> --pub <update-manifest.pub.pem>'
    );
  }

  const builtinPem = readBuiltinPublicKeyPem();
  const builtinBody = pemBody(builtinPem);

  // 1) 产物公钥 == 内置信任公钥（私钥派生 ⇔ secret 配对）
  const artifactPubPem = readFileSync(pubPath, 'utf8').trim();
  const artifactBody = pemBody(artifactPubPem);
  if (artifactBody !== builtinBody) {
    throw new Error(
      '[verify] FAIL: update-manifest.pub.pem 与客户端内置信任公钥不一致！\n'
        + `  artifact : ${artifactBody}\n`
        + `  builtin  : ${builtinBody}\n`
        + 'SPARK_PLUGIN_SIGNING_PRIVATE_KEY 与本机 .secrets/spark-update-signing-private-key.pem 不是同一把，'
        + '客户端将无法验签。请到 GitHub → Settings → Secrets and variables 修正后重新 tag 发布。'
    );
  }

  // 2) 内置公钥 Ed25519 验签 manifest 分离签名（复现客户端 verify_manifest_signature）
  const manifestText = readFileSync(manifestPath, 'utf8');
  const sigBase64 = readFileSync(sigPath, 'utf8').trim();
  let publicKey;
  try {
    publicKey = createPublicKey({
      key: Buffer.from(artifactBody, 'base64'),
      format: 'der',
      type: 'spki',
    });
  } catch (error) {
    throw new Error(`[verify] FAIL: 内置公钥无法解析（${error.message}）`);
  }
  const ok = verify(
    null,
    Buffer.from(manifestText, 'utf8'),
    publicKey,
    Buffer.from(sigBase64, 'base64')
  );
  if (!ok) {
    throw new Error(
      '[verify] FAIL: Ed25519 验签未通过（manifest 与 sig 不匹配或签名损坏），资产不可发布'
    );
  }

  console.log(`[verify] OK: 产物公钥 == 内置信任公钥，Ed25519 验签通过（${path.basename(manifestPath)}）`);
}

try {
  main();
} catch (error) {
  console.error('[verify] failed:', error.message);
  process.exit(1);
}
