/**
 * dist 产物自洽校验（plugin-dist §9.4）：安装包 = dist 全量打进，
 * dist 文本产物中不允许残留对 vendor/ 或 dist 之外路径的悬空引用
 * （"依赖全量打进、运行时禁止外部拉取代码"红线的构建期防线）。
 *
 * 扫描 dist/** 代码类文本产物（.js/.mjs/.cjs/.jsx/.ts/.tsx/.vue/.html），
 * 抽取静态 import / export-from / 动态 import() / require() 的相对路径
 * 引用（./ 或 ../ 开头），命中以下任一即抛 E_DIST_NOT_SELFCONTAINED
 * （fail-closed，构建中止）：
 * 1. 引用路径段含 vendor（vendor 不随包分发，引用即悬空）；
 * 2. 解析后越过 dist 根目录（指向工程内源码或包外路径）；
 * 3. 解析在 dist 内但目标文件不存在（含 .js/.mjs 与 /index.js 兜底）。
 */

import fs from 'fs';
import { readdir, readFile } from 'fs/promises';
import path from 'path';
import { ManifestError } from './manifest.js';

const SCANNABLE_EXTENSIONS = new Set(['.js', '.mjs', '.cjs', '.jsx', '.ts', '.tsx', '.vue', '.html']);

// 静态/动态 import、export-from、require 的字符串字面量 specifier
const SPECIFIER_RE =
  /(?:import\s+(?:[^'"(]*?\s+from\s+)?|export\s+[^'"]*?\s+from\s+|import\s*\(\s*|require\s*\(\s*)['"]([^'"]+)['"]/g;

async function walkDistFiles(distDir) {
  const walk = async (dir) => {
    const entries = await readdir(dir, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        files.push(...(await walk(fullPath)));
      } else if (entry.isFile() && SCANNABLE_EXTENSIONS.has(path.extname(entry.name).toLowerCase())) {
        files.push(fullPath);
      }
    }
    return files;
  };
  return walk(distDir);
}

function resolveInsideDist(fileDir, specifier) {
  const candidates = [specifier, `${specifier}.js`, `${specifier}.mjs`, `${specifier}/index.js`];
  for (const candidate of candidates) {
    if (fs.existsSync(path.resolve(fileDir, candidate))) {
      return true;
    }
  }
  return false;
}

/**
 * @param {string} distDir 插件工程的 dist 目录绝对路径
 * @throws {ManifestError} E_DIST_NOT_SELFCONTAINED
 */
export async function assertDistSelfContained(distDir) {
  if (!fs.existsSync(distDir)) {
    return; // dist 缺失由后续打包环节报 E_DIST_MISSING / 缺 manifest 错误
  }
  const violations = [];
  for (const filePath of await walkDistFiles(distDir)) {
    const content = await readFile(filePath, 'utf8');
    const relFile = path.relative(distDir, filePath).split(path.sep).join('/');
    for (const match of content.matchAll(SPECIFIER_RE)) {
      const specifier = match[1];
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) {
        continue; // 裸说明符（node_modules 解析）不属本检查面
      }
      const segments = specifier.split('/');
      if (segments.includes('vendor')) {
        violations.push(`${relFile} → ${specifier}（引用 vendor/，vendor 不随包分发）`);
        continue;
      }
      const resolved = path.resolve(path.dirname(filePath), specifier);
      const rel = path.relative(distDir, resolved);
      if (rel.startsWith('..') || path.isAbsolute(rel)) {
        violations.push(`${relFile} → ${specifier}（引用越过 dist 根目录，目标不进安装包）`);
        continue;
      }
      if (!resolveInsideDist(path.dirname(filePath), specifier)) {
        violations.push(`${relFile} → ${specifier}（dist 内不存在该目标文件）`);
      }
    }
  }
  if (violations.length > 0) {
    throw new ManifestError(
      'E_DIST_NOT_SELFCONTAINED',
      `dist 产物存在对外部路径的悬空引用（依赖须全量打进安装包）：\n  - ${violations.join('\n  - ')}`
    );
  }
}
