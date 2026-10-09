/**
 * 库包构建（--mode library）：产物为可发布到 npm 或 git 仓库引用的源码包目录
 * （Spark 不自建包仓库，runtime-and-trust §4.1-6）。
 *
 * 产物布局（<outputDir>/<slug>-<version>/）：
 * - package.json    npm 发布用清单（name 由 manifest.id 派生，files 限定发布面）
 * - manifest.json   原样插件清单（kind:"library"）
 * - sbom.json       本库自身依赖的 SBOM（无依赖时 libraries 为空数组）
 * - dist/**         库产物（须先经插件自身 build 生成）
 * 同级另有 <slug>-<version>-checksums.txt 校验清单。
 */

import fs from 'fs';
import { cp, mkdir, readdir, readFile, writeFile } from 'fs/promises';
import path from 'path';
import { sha256Hex } from './lockfile.js';
import { renderSbom } from './sbom.js';
import { assertDistSelfContained } from './dist-scan.js';

function npmSafeName(id) {
  const name = String(id).toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '');
  return name || 'spark-library';
}

async function walkFiles(rootDir) {
  const walk = async (dir) => {
    const entries = await readdir(dir, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        files.push(...(await walk(fullPath)));
      } else if (entry.isFile()) {
        files.push(path.relative(rootDir, fullPath).split(path.sep).join('/'));
      }
    }
    return files;
  };
  return (await walk(rootDir)).sort();
}

/**
 * @returns {Promise<{packageDir, checksumsPath, files}>}
 */
export async function buildLibraryPackage({ pluginDir, manifest, sbom, outputDir }) {
  const id = manifest.id || path.basename(pluginDir);
  const version = manifest.version;
  if (!version) {
    throw new Error(`E_MANIFEST_INVALID: library manifest missing version (${pluginDir})`);
  }
  const distDir = path.join(pluginDir, 'dist');
  if (!fs.existsSync(distDir)) {
    throw new Error(`E_DIST_MISSING: 缺少 ${distDir}，请先运行库的构建脚本生成产物`);
  }
  // 库包同样只发 dist：残留对 vendor/ 或 dist 外路径的引用即拒（fail-closed）
  await assertDistSelfContained(distDir);

  const slug = npmSafeName(id);
  const packageDir = path.join(outputDir, `${slug}-${version}`);
  await mkdir(packageDir, { recursive: true });

  await cp(distDir, path.join(packageDir, 'dist'), { recursive: true });
  await writeFile(
    path.join(packageDir, 'manifest.json'),
    JSON.stringify(manifest, null, 2) + '\n',
    'utf8'
  );
  await writeFile(path.join(packageDir, 'sbom.json'), renderSbom(sbom), 'utf8');
  const packageJson = {
    name: slug,
    version,
    type: 'module',
    description: manifest.description ?? '',
    files: ['dist', 'manifest.json', 'sbom.json'],
    sparkPlugin: { kind: 'library', id }
  };
  await writeFile(
    path.join(packageDir, 'package.json'),
    JSON.stringify(packageJson, null, 2) + '\n',
    'utf8'
  );

  const files = await walkFiles(packageDir);
  const checksumLines = [];
  for (const relPath of files) {
    const content = await readFile(path.join(packageDir, relPath));
    checksumLines.push(`${sha256Hex(content)}  ${slug}-${version}/${relPath}`);
  }
  const checksumsPath = path.join(outputDir, `${slug}-${version}-checksums.txt`);
  await writeFile(checksumsPath, checksumLines.join('\n') + '\n', 'utf8');

  return { packageDir, checksumsPath, files };
}
