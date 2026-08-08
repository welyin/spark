#!/usr/bin/env node

/**
 * ai-chat 产物收尾脚本（vite build 之后运行，build:ai-chat 串联第二步）：
 * 1. 拷贝 manifest.json 到 dist/；
 * 2. lib 模式抽出的样式表（dist/style.css）归入 dist/assets/main.css；
 * 3. dist 结构自检：views/main.js 为非空 ESM、manifest.json 存在且可解析。
 */

import { access, mkdir, readFile, rename, rm, cp } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const pluginRoot = path.resolve(__dirname, '..', 'ai-chat');
const distDir = path.join(pluginRoot, 'dist');

const VIEW_BUNDLES = ['main'];

function fail(message) {
  console.error(`[ai-chat-dist] 自检失败：${message}`);
  process.exit(1);
}

async function exists(target) {
  try {
    await access(target);
    return true;
  } catch {
    return false;
  }
}

async function checkViewBundle(name) {
  const bundlePath = path.join(distDir, 'views', `${name}.js`);
  if (!(await exists(bundlePath))) {
    fail(`缺少 dist/views/${name}.js（vite lib 构建产物）`);
  }
  const bundle = await readFile(bundlePath, 'utf8');
  if (bundle.length === 0) {
    fail(`dist/views/${name}.js 为空文件`);
  }
  if (!/export\s*[{]/.test(bundle) && !/export\s+default/.test(bundle)) {
    fail(`dist/views/${name}.js 不含 ESM 导出语法，产物形态异常`);
  }
  if (/^\s*(const|var|let)\s+\S+\s*=\s*require\(/m.test(bundle)) {
    fail(`dist/views/${name}.js 疑似 CJS 产物（含顶层 require）`);
  }
}

async function main() {
  // 1. manifest.json → dist/manifest.json
  const manifestSource = path.join(pluginRoot, 'manifest.json');
  const manifestTarget = path.join(distDir, 'manifest.json');
  await mkdir(distDir, { recursive: true });
  await cp(manifestSource, manifestTarget);

  // 2. lib 模式抽出的样式表归入 dist/assets/main.css
  const extractedCss = path.join(distDir, 'style.css');
  if (await exists(extractedCss)) {
    await mkdir(path.join(distDir, 'assets'), { recursive: true });
    const cssTarget = path.join(distDir, 'assets', 'main.css');
    await rm(cssTarget, { force: true });
    await rename(extractedCss, cssTarget);
  }

  // 3. dist 结构自检
  for (const name of VIEW_BUNDLES) {
    await checkViewBundle(name);
  }

  const manifestText = await readFile(manifestTarget, 'utf8');
  let manifest;
  try {
    manifest = JSON.parse(manifestText);
  } catch (error) {
    fail(`dist/manifest.json 不是合法 JSON：${error.message}`);
  }
  if (manifest.id !== 'ai-chat') {
    fail(`dist/manifest.json id 异常：${manifest.id}`);
  }

  console.log(
    `[ai-chat-dist] dist 就绪：manifest.json + ${VIEW_BUNDLES.map((name) => `views/${name}.js`).join(' + ')}` +
      (await exists(path.join(distDir, 'assets')) ? ' + assets/' : '')
  );
}

main().catch((error) => {
  console.error('[ai-chat-dist] failed', error);
  process.exit(1);
});
