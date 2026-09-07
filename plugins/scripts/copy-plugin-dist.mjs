#!/usr/bin/env node

/**
 * 插件产物收尾脚本（vite build 之后运行，build:<id> 串联第二步）。
 * copy-example-dist.mjs 等的通用化版本（C11 起新插件统一走本脚本）：
 *
 *   node scripts/copy-plugin-dist.mjs --pluginId spark-affairs --views main,affair-card
 *
 * 步骤：
 * 1. 拷贝 manifest.json（与可选 assets/ 静态资源）到 dist/；
 * 2. lib 模式抽出的样式表（dist/style.css）归入 dist/assets/main.css；
 * 3. dist 结构自检：每个 views/<name>.js 为非空 ESM、manifest.json 存在且
 *    可解析、manifest id 与插件目录一致——任一不满足即非零退出，阻断打包。
 */

import { access, mkdir, readFile, rename, rm, cp } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

function fail(message) {
  console.error(`[plugin-dist] 自检失败：${message}`);
  process.exit(1);
}

function argValue(name) {
  const index = process.argv.indexOf(`--${name}`);
  if (index === -1 || index + 1 >= process.argv.length) {
    fail(`缺少参数 --${name}`);
  }
  return process.argv[index + 1];
}

const pluginId = argValue('pluginId');
const views = argValue('views').split(',').map((item) => item.trim()).filter(Boolean);
if (views.length === 0) {
  fail('--views 至少列出一个视图 bundle（通常含 main）');
}

const pluginRoot = path.resolve(__dirname, '..', pluginId);
const distDir = path.join(pluginRoot, 'dist');

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
  // ESM 正向断言：lib 模式 ES 产物必须含导出语法（与 copy-example-dist 同口径）
  if (!/export\s*[{]/.test(bundle) && !/export\s+default/.test(bundle)) {
    fail(`dist/views/${name}.js 不含 ESM 导出语法，产物形态异常`);
  }
  if (/^\s*(const|var|let)\s+\S+\s*=\s*require\(/m.test(bundle)) {
    fail(`dist/views/${name}.js 疑似 CJS 产物（含顶层 require）`);
  }
}

async function main() {
  // 1. manifest.json → dist/manifest.json（唯一事实源，与 bundle 并列分发）
  const manifestSource = path.join(pluginRoot, 'manifest.json');
  const manifestTarget = path.join(distDir, 'manifest.json');
  await mkdir(distDir, { recursive: true });
  await cp(manifestSource, manifestTarget);

  // 2. 静态资源目录（可选）→ dist/assets/
  const assetsSource = path.join(pluginRoot, 'assets');
  if (await exists(assetsSource)) {
    await cp(assetsSource, path.join(distDir, 'assets'), { recursive: true });
  }

  // 3. lib 模式抽出的样式表归入 dist/assets/main.css
  const extractedCss = path.join(distDir, 'style.css');
  if (await exists(extractedCss)) {
    await mkdir(path.join(distDir, 'assets'), { recursive: true });
    const cssTarget = path.join(distDir, 'assets', 'main.css');
    await rm(cssTarget, { force: true });
    await rename(extractedCss, cssTarget);
  }

  // 4. dist 结构自检（全部视图 bundle + manifest）
  for (const name of views) {
    await checkViewBundle(name);
  }

  const manifestText = await readFile(manifestTarget, 'utf8');
  let manifest;
  try {
    manifest = JSON.parse(manifestText);
  } catch (error) {
    fail(`dist/manifest.json 不是合法 JSON：${error.message}`);
  }
  if (manifest.id !== pluginId) {
    fail(`dist/manifest.json id 异常：${manifest.id}（期望 ${pluginId}）`);
  }

  console.log(
    `[plugin-dist] ${pluginId} dist 就绪：manifest.json + ${views.map((name) => `views/${name}.js`).join(' + ')}` +
      (await exists(path.join(distDir, 'assets')) ? ' + assets/' : '')
  );
}

main().catch((error) => {
  console.error('[plugin-dist] failed', error);
  process.exit(1);
});
