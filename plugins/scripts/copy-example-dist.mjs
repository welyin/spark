#!/usr/bin/env node

/**
 * spark-example 产物收尾脚本（vite build 之后运行，build:example 串联第二步）：
 * 1. 拷贝 manifest.json 与静态资源（assets/，若存在）到 dist/；
 * 2. lib 模式抽出的样式表（dist/style.css）归入 dist/assets/main.css；
 * 3. dist 结构自检：views/main.js 与 views/post-card.js 均为非空 ESM、
 *    manifest.json 存在且可解析、manifest id 与插件目录一致——
 *    任一不满足即非零退出，阻断后续 .spkg 打包。
 */

import { access, mkdir, readFile, rename, rm, cp } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const pluginRoot = path.resolve(__dirname, '..', 'spark-example');
const distDir = path.join(pluginRoot, 'dist');

/** 多视图 bundle（设计文档「包形态」：views/main.js + views/<viewId>.js） */
const VIEW_BUNDLES = ['main', 'post-card'];

function fail(message) {
  console.error(`[example-dist] 自检失败：${message}`);
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
  if (!/export\s*[{]/.test(bundle) && !/export\s+default/.test(bundle) && !/\bimport\(/.test(bundle) && !/\bimport\s/.test(bundle)) {
    fail(`dist/views/${name}.js 不含 ESM 语法，产物形态异常`);
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

  // 4. dist 结构自检（全部视图 bundle）
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
  if (manifest.id !== 'spark-example') {
    fail(`dist/manifest.json id 异常：${manifest.id}`);
  }

  console.log(
    `[example-dist] dist 就绪：manifest.json + ${VIEW_BUNDLES.map((name) => `views/${name}.js`).join(' + ')}` +
      (await exists(path.join(distDir, 'assets')) ? ' + assets/' : '')
  );
}

main().catch((error) => {
  console.error('[example-dist] failed', error);
  process.exit(1);
});
