#!/usr/bin/env node

/**
 * spark-moments 产物收尾脚本（vite build 之后运行，build:moments 串联第二步）。
 * 对齐 scripts/copy-example-dist.mjs 形态：拷贝 manifest.json → dist/，样式表归位，
 * 自检 main + notify-card + background 三个视图 bundle 均为非空 ESM。
 */

import { access, mkdir, readFile, rename, rm, cp } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const pluginRoot = path.resolve(__dirname, '..', 'spark-moments');
const distDir = path.join(pluginRoot, 'dist');

/** 多视图 bundle（views/main.js + views/<viewId>.js + background.js） */
const VIEW_BUNDLES = ['main', 'notify-card', 'background'];

function fail(message) {
  console.error('[moments-dist] 自检失败：' + message);
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

/**
 * 校验单个视图 bundle。
 * - expectExport=true  （notify-card.js）：导出型视图，额外要求含 ESM 导出语法；
 * - expectExport=false （main.js / background.js）：纯自执行入口，无导出预期
 *   （main 只做引导、background 是 QuickJS 零依赖脚本，均为合法 ESM 入口）。
 * 两类都做「非空 + 非 CJS 顶层 require」负向检查兜底形态异常。
 */
async function checkViewBundle(name, expectExport) {
  const bundlePath = path.join(distDir, 'views', name + '.js');
  if (!(await exists(bundlePath))) {
    fail('缺少 dist/views/' + name + '.js（vite lib 构建产物）');
  }
  const bundle = await readFile(bundlePath, 'utf8');
  if (bundle.length === 0) {
    fail('dist/views/' + name + '.js 为空文件');
  }
  if (expectExport && !/export\s*[{]/.test(bundle) && !/export\s+default/.test(bundle)) {
    fail('dist/views/' + name + '.js 不含 ESM 导出语法，产物形态异常');
  }
  if (/^\s*(const|var|let)\s+\S+\s*=\s*require\(/m.test(bundle)) {
    fail('dist/views/' + name + '.js 疑似 CJS 产物（含顶层 require）');
  }
}

async function main() {
  // 1. manifest.json → dist/manifest.json（唯一事实源）
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

  // 4. dist 结构自检（main/background 为纯自执行入口，notify-card 为导出型视图）
  await checkViewBundle('main', false);
  await checkViewBundle('notify-card', true);
  await checkViewBundle('background', false);

  const manifestText = await readFile(manifestTarget, 'utf8');
  let manifest;
  try {
    manifest = JSON.parse(manifestText);
  } catch (error) {
    fail('dist/manifest.json 不是合法 JSON：' + error.message);
  }
  if (manifest.id !== 'spark-moments') {
    fail('dist/manifest.json id 异常：' + manifest.id);
  }

  console.log('[moments-dist] dist 就绪：manifest.json + ' + VIEW_BUNDLES.map((name) => 'views/' + name + '.js').join(' + ') + (await exists(path.join(distDir, 'assets')) ? ' + assets/' : ''));
}

main().catch((error) => {
  console.error('[moments-dist] failed', error);
  process.exit(1);
});
