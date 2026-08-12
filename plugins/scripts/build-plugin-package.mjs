#!/usr/bin/env node

/**
 * 插件打包脚本（build-example-package.mjs 的泛化版，按 pluginId 读对应目录与声明）。
 *
 * 产物（默认输出到 code/app/dist-market/plugins/<pluginId>/）：
 * - spark-plugin-<pluginId>-<version>.spkg  JSON 包：{pluginId, domain, version, files:[{path, sha256, size, contentBase64}]}
 * - update-manifest.json                  更新清单（市场服务消费）
 * - update-manifest.sig                   Ed25519 分离签名（base64）
 * - update-manifest.pub.pem               签名公钥（SPKI PEM，便于核对信任链）
 * - plugin-checksums.txt                  sha256 校验清单
 *
 * 用法：
 *   node plugins/scripts/build-plugin-package.mjs \
 *     --pluginId spark-moments \
 *     --version 0.1.0 \
 *     --repository welyin/spark \
 *     --releaseTag spark-moments-v0.1.0 \
 *     --outputDir app/dist-market/plugins/spark-moments
 *
 * 参数说明：
 * - --pluginId 必填：插件目录名（spark-moments / ai-chat / spark-example），
 *   默认从该插件 manifest.json 读取 id/domain/version 兜底，避免与清单脱钩；
 * - --version / --pluginDomain 可选覆盖（不传则取自 <pluginId>/manifest.json）；
 * - --repository / --releaseTag 提供时，manifest 资产 url 指向 GitHub release
 *   asset（https://github.com/<repository>/releases/download/<releaseTag>/<file>）；
 *   缺省则回落到 file:// 本地路径（本地联调）。
 *
 * 签名私钥（按优先级）：
 * 1. 环境变量 SPARK_PLUGIN_SIGNING_PRIVATE_KEY（PEM 内容）
 * 2. <code>/.secrets/spark-update-signing-private-key.pem（新约定）
 * 3. <workspace>/.secrets/spark-update-signing-private-key.pem
 * 4. <workspace>/desktop/.secrets/spark-update-signing-private-key.pem（旧仓库约定，只读沿用）
 */

import { createHash, createPrivateKey, createPublicKey, sign } from 'crypto';
import { mkdir, readdir, readFile, writeFile } from 'fs/promises';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
// code/plugins/scripts → code/plugins → code → <workspace root>
const pluginsRoot = path.resolve(__dirname, '..');
const codeRoot = path.resolve(pluginsRoot, '..');
const workspaceRoot = path.resolve(codeRoot, '..');

const OFFICIAL_PLUGIN_IDS = ['spark-moments', 'ai-chat'];

/**
 * 打进 .spkg 的插件产物文件（dist/ 全量，deterministic 顺序）。
 * dist 由 build:<id> 生成（vite ESM bundle + manifest.json + assets/），
 * 本脚本不再直接收集 TS/Vue 源码。
 */
async function collectDistFiles(pluginId) {
  const distDir = path.join(pluginsRoot, pluginId, 'dist');
  if (!fs.existsSync(path.join(distDir, 'manifest.json'))) {
    throw new Error(
      `缺少 ${distDir}（含 manifest.json），请先运行对应插件的 build 脚本（如 npm run build:moments）生成插件产物`
    );
  }
  const walk = async (dir) => {
    const entries = await readdir(dir, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        files.push(...(await walk(fullPath)));
      } else {
        files.push(path.relative(distDir, fullPath).split(path.sep).join('/'));
      }
    }
    return files;
  };
  return (await walk(distDir)).sort();
}

const PRIVATE_KEY_FALLBACK_PATHS = [
  path.join(codeRoot, '.secrets', 'spark-update-signing-private-key.pem'),
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
    return '';
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
      + '.secrets/spark-update-signing-private-key.pem (code/.secrets, workspace root or legacy desktop/.secrets)'
  );
}

async function readPluginManifest(pluginId) {
  const manifestPath = path.join(pluginsRoot, pluginId, 'manifest.json');
  if (!fs.existsSync(manifestPath)) {
    throw new Error(`缺少插件清单 ${manifestPath}`);
  }
  return JSON.parse(await readFile(manifestPath, 'utf8'));
}

async function main() {
  const args = parseArgs(process.argv);

  const pluginId = args.pluginId;
  if (!pluginId) {
    throw new Error('Missing --pluginId. 示例：--pluginId spark-moments（或 ai-chat）');
  }
  if (!fs.existsSync(path.join(pluginsRoot, pluginId))) {
    throw new Error(`插件目录不存在：code/plugins/${pluginId}`);
  }

  const pluginRoot = path.join(pluginsRoot, pluginId);

  // 从插件清单兜底 id / domain / version，避免与清单脱钩（manifest.json 为唯一事实源）
  const manifest = await readPluginManifest(pluginId);
  const effectiveId = manifest.id || pluginId;
  const domain = args.pluginDomain ?? (manifest.domain ?? `plugin:${effectiveId}`);
  const version = normalizeVersion(args.version) || normalizeVersion(manifest.version);

  if (!version) {
    throw new Error(`无法确定版本：--version 未传且 ${pluginId}/manifest.json 缺少 version`);
  }
  if (effectiveId !== pluginId) {
    // 目录名与清单 id 不一致时显式告警（manifest.json 是唯一事实源，以其为准）
    console.warn(
      `[plugin-package] 提示：插件目录 ${pluginId} 的 manifest.json id 为 ${effectiveId}，`
      + '包内 pluginId 以清单为准（与市场校验一致）'
    );
  }

  const outputDir = args.outputDir
    ? path.resolve(codeRoot, args.outputDir)
    : path.join(codeRoot, 'app', 'dist-market', 'plugins', pluginId);
  const repository = args.repository ?? process.env.GITHUB_REPOSITORY ?? '';
  const releaseTag = args.releaseTag ?? '';

  await mkdir(outputDir, { recursive: true });

  const sourceFiles = await collectDistFiles(pluginId);
  const distDir = path.join(pluginRoot, 'dist');

  const bundledFiles = [];
  for (const relativePath of sourceFiles) {
    const sourcePath = path.join(distDir, relativePath);
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

  const packageFileName = `spark-plugin-${effectiveId}-${version}.spkg`;
  const packagePath = path.join(outputDir, packageFileName);
  const packagePayload = {
    pluginId: effectiveId,
    domain,
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
    pluginId: effectiveId,
    domain,
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

  console.log(
    `[plugin-package] generated ${manifestPath}`
    + (OFFICIAL_PLUGIN_IDS.includes(pluginId) ? '' : `（插件 ${pluginId} 非官方发布目录，请核对）`)
  );
}

main().catch((error) => {
  console.error('[plugin-package] failed', error);
  process.exit(1);
});
