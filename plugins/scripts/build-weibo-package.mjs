#!/usr/bin/env node

/**
 * 插件打包脚本（移植自旧 desktop/scripts/plugins/build-weibo-package.mjs）。
 *
 * 产物（默认输出到 code/app/dist-market/plugins/<pluginId>/）：
 * - spark-plugin-<pluginId>-<version>.spkg  JSON 包：{pluginId, domain, version, files:[{path, sha256, size, contentBase64}]}
 * - update-manifest.json                  更新清单（市场服务消费）
 * - update-manifest.sig                   Ed25519 分离签名（base64）
 * - update-manifest.pub.pem               签名公钥（SPKI PEM，便于核对信任链）
 * - plugin-checksums.txt                  sha256 校验清单
 *
 * 签名私钥（按优先级）：
 * 1. 环境变量 SPARK_PLUGIN_SIGNING_PRIVATE_KEY（PEM 内容）
 * 2. <workspace>/.secrets/spark-update-signing-private-key.pem（新约定）
 * 3. <workspace>/desktop/.secrets/spark-update-signing-private-key.pem（旧仓库约定，只读沿用）
 */

import { createHash, createPrivateKey, createPublicKey, sign } from 'crypto';
import { mkdir, readFile, writeFile } from 'fs/promises';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
// code/plugins/scripts → code/plugins → code → <workspace root>
const pluginsRoot = path.resolve(__dirname, '..');
const codeRoot = path.resolve(pluginsRoot, '..');
const workspaceRoot = path.resolve(codeRoot, '..');

/** 打进 .spkg 的插件源文件（插件运行全量，deterministic 顺序）。 */
const sourceFiles = [
  'manifest.ts',
  'index.ts',
  'model.ts',
  'service.ts',
  'WeiboCoreView.vue'
];

const PRIVATE_KEY_FALLBACK_PATHS = [
  path.join(workspaceRoot, '.secrets', 'spark-update-signing-private-key.pem'),
  path.join(workspaceRoot, 'desktop', '.secrets', 'spark-update-signing-private-key.pem')
];

function parseArgs(argv) {
  const args = {};
  for (let i = 2; i < argv.length; i += 1) {
    const part = argv[i];
    if (!part.startsWith('--')) {
      continue;
    }
    const key = part.slice(2);
    const value = argv[i + 1];
    args[key] = value;
    i += 1;
  }
  return args;
}

function normalizeVersion(value) {
  if (!value) {
    return '0.1.0';
  }
  return value.startsWith('v') ? value.slice(1) : value;
}

function buildReleaseAssetUrl(repository, tag, fileName) {
  return `https://github.com/${repository}/releases/download/${tag}/${fileName}`;
}

function sha256(content) {
  return createHash('sha256').update(content).digest('hex');
}

async function readSigningPrivateKey() {
  const fromEnv = process.env.SPARK_PLUGIN_SIGNING_PRIVATE_KEY?.trim();
  if (fromEnv) {
    return fromEnv;
  }
  for (const candidate of PRIVATE_KEY_FALLBACK_PATHS) {
    if (fs.existsSync(candidate)) {
      return await readFile(candidate, 'utf8');
    }
  }
  throw new Error(
    'Missing plugin signing private key. Set SPARK_PLUGIN_SIGNING_PRIVATE_KEY or provide '
      + '.secrets/spark-update-signing-private-key.pem (workspace root or legacy desktop/.secrets)'
  );
}

async function main() {
  const args = parseArgs(process.argv);

  const pluginId = args.pluginId ?? 'weibo-core';
  const pluginDomain = args.pluginDomain ?? 'plugin:weibo-core';
  const version = normalizeVersion(args.version);
  const pluginRoot = path.join(pluginsRoot, pluginId);
  const outputDir = args.outputDir
    ? path.resolve(codeRoot, args.outputDir)
    : path.join(codeRoot, 'app', 'dist-market', 'plugins', pluginId);
  const repository = args.repository ?? process.env.GITHUB_REPOSITORY ?? '';
  const releaseTag = args.releaseTag ?? '';

  await mkdir(outputDir, { recursive: true });

  const bundledFiles = [];
  for (const relativePath of sourceFiles) {
    const sourcePath = path.join(pluginRoot, relativePath);
    const content = await readFile(sourcePath);
    const digest = sha256(content);

    const targetPath = path.join(outputDir, relativePath);
    await mkdir(path.dirname(targetPath), { recursive: true });
    await writeFile(targetPath, content);

    bundledFiles.push({
      path: relativePath,
      sha256: digest,
      size: content.byteLength,
      contentBase64: content.toString('base64')
    });
  }

  const packageFileName = `spark-plugin-${pluginId}-${version}.spkg`;
  const packagePath = path.join(outputDir, packageFileName);
  const packagePayload = {
    pluginId,
    domain: pluginDomain,
    version,
    files: bundledFiles
  };
  const packageBuffer = Buffer.from(JSON.stringify(packagePayload, null, 2) + '\n', 'utf8');
  await writeFile(packagePath, packageBuffer);

  const packageDigest = sha256(packageBuffer);
  const packageSize = packageBuffer.byteLength;
  const packageUrl = repository && releaseTag
    ? buildReleaseAssetUrl(repository, releaseTag, packageFileName)
    : `file://${packagePath}`;

  const updateManifest = {
    pluginId,
    domain: pluginDomain,
    manifestVersion: 1,
    version,
    releaseTime: new Date().toISOString(),
    assets: [
      {
        kind: 'package',
        fileName: packageFileName,
        url: packageUrl,
        sha256: packageDigest,
        size: packageSize
      }
    ]
  };

  const manifestText = JSON.stringify(updateManifest, null, 2) + '\n';
  const manifestPath = path.join(outputDir, 'update-manifest.json');
  await writeFile(manifestPath, manifestText, 'utf8');

  const privateKeyPem = await readSigningPrivateKey();
  const privateKey = createPrivateKey(privateKeyPem);
  const signature = sign(null, Buffer.from(manifestText, 'utf8'), privateKey).toString('base64');
  await writeFile(path.join(outputDir, 'update-manifest.sig'), signature + '\n', 'utf8');
  const publicPem = createPublicKey(privateKey).export({ type: 'spki', format: 'pem' }).toString();
  await writeFile(path.join(outputDir, 'update-manifest.pub.pem'), publicPem, 'utf8');

  const checksums = [
    `${packageDigest}  ${packageFileName}`,
    `${sha256(Buffer.from(manifestText, 'utf8'))}  update-manifest.json`
  ];
  await writeFile(path.join(outputDir, 'plugin-checksums.txt'), `${checksums.join('\n')}\n`, 'utf8');

  console.log('[plugin-package] generated', manifestPath);
}

main().catch((error) => {
  console.error('[plugin-package] failed', error);
  process.exit(1);
});
