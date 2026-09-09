#!/usr/bin/env node

/**
 * 默认内置插件打包（communication §4.2，A19）：把 spark-chat / spark-contacts
 * 的 dist 打成 .spkg 直出到 code/app/src-tauri/resources/builtin-plugins/——
 * 生产构建经 tauri.conf bundle.resources 打入应用资源目录，首跑由
 * src-tauri market/builtin.rs 预装为市场记录（trust="builtin"）；
 * dev 链路同目录被启动对账直接读取（市场记录是桥授权的数据源）。
 *
 * 前置：先跑 npm run build:chat / build:contacts 生成各插件 dist。
 * 产物为构建中间物（.gitignore），不签名——信任模型 trust="builtin"
 * （与应用同体分发，无需发布侧签名链）。
 */

import { createHash } from 'crypto';
import { mkdir, readdir, readFile, rm, writeFile } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const pluginsRoot = path.resolve(__dirname, '..');
const outputDir = path.resolve(pluginsRoot, '../app/src-tauri/resources/builtin-plugins');

const BUILTIN_PLUGIN_IDS = ['spark-chat', 'spark-contacts'];

async function collectDistFiles(pluginId) {
  const distDir = path.join(pluginsRoot, pluginId, 'dist');
  const entries = [];
  async function walk(dir, rel) {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const abs = path.join(dir, entry.name);
      const relPath = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        await walk(abs, relPath);
      } else if (entry.isFile()) {
        entries.push({ abs, relPath });
      }
    }
  }
  await walk(distDir, '');
  // deterministic 顺序（与 build-plugin-package.mjs 同口径）
  entries.sort((a, b) => (a.relPath < b.relPath ? -1 : 1));
  return entries.map(({ abs, relPath }) => ({ abs, relPath }));
}

async function buildOne(pluginId) {
  const distDir = path.join(pluginsRoot, pluginId, 'dist');
  const manifest = JSON.parse(await readFile(path.join(distDir, 'manifest.json'), 'utf8'));
  const files = [];
  for (const { abs, relPath } of await collectDistFiles(pluginId)) {
    const content = await readFile(abs);
    files.push({
      path: relPath.split(path.sep).join('/'),
      sha256: createHash('sha256').update(content).digest('hex'),
      size: content.length,
      contentBase64: content.toString('base64')
    });
  }
  const container = {
    pluginId: manifest.id,
    domain: manifest.domain,
    version: manifest.version,
    files
  };
  const target = path.join(outputDir, `${manifest.id}-${manifest.version}.spkg`);
  await writeFile(target, JSON.stringify(container));
  console.log(`[builtin-pkg] ${manifest.id}@${manifest.version} → ${path.relative(process.cwd(), target)}`);
}

async function main() {
  await mkdir(outputDir, { recursive: true });
  // 清掉旧版本内置包（同插件多版本并存会让对账逐包处理，旧包重复安装/升级抖动）
  for (const entry of await readdir(outputDir)) {
    if (entry.endsWith('.spkg')) {
      await rm(path.join(outputDir, entry));
    }
  }
  for (const pluginId of BUILTIN_PLUGIN_IDS) {
    await buildOne(pluginId);
  }
}

main().catch((error) => {
  console.error('[builtin-pkg] failed', error);
  process.exit(1);
});
