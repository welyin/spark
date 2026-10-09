/**
 * spark-plugin-cli 命令分发：
 * - lock    计算各 library 的 vendor 树哈希，写 spark-libraries.lock.json 并回填 manifest hash；
 * - build   双产物构建：--mode app（安装包 .spkg + SBOM + 锚定签名，复用
 *           plugins/scripts/build-plugin-package.mjs）/ --mode library（npm/git 形态库包）；
 *           构建前对 dist 文本产物做自洽扫描，残留对 vendor/ 或 dist 外路径的引用即拒
 *           （E_DIST_NOT_SELFCONTAINED，见 dist-scan.js）；
 * - verify  只校验 manifest 扩展字段与锁文件、vendor 树完整性（CI 用）。
 *
 * 用法：
 *   node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs lock   --pluginId spark-moments
 *   node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs build  --pluginId spark-moments --mode app \
 *     [--repository welyin/spark --releaseTag spark-moments-v0.1.0] [--outputDir <dir>]
 *   node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs build  --dir <pluginDir> --mode library
 *   node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs verify --pluginId spark-moments
 *
 * 定位插件：--pluginId <id>（code/plugins/<id>）或 --dir <任意插件工程目录>。
 */

import { spawnSync } from 'child_process';
import fs from 'fs';
import { mkdir, readFile, writeFile } from 'fs/promises';
import path from 'path';
import { fileURLToPath } from 'url';
import { ManifestError, normalizeRepoId, validateManifestExtensions } from './manifest.js';
import {
  LockError,
  hashVendorTree,
  readLock,
  verifyLock,
  vendorPathForRepo,
  writeLock
} from './lockfile.js';
import { SBOM_FILE_NAME, buildSbom, renderSbom } from './sbom.js';
import { buildLibraryPackage } from './build-library.js';
import { assertDistSelfContained } from './dist-scan.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
// packages/spark-plugin-cli/src → packages/spark-plugin-cli → packages → code
const codeRoot = path.resolve(__dirname, '..', '..', '..');
const pluginsRoot = path.join(codeRoot, 'plugins');
const packagerScript = path.join(pluginsRoot, 'scripts', 'build-plugin-package.mjs');

const toolVersion = JSON.parse(
  fs.readFileSync(path.resolve(__dirname, '..', 'package.json'), 'utf8')
).version;

function parseArgs(argv) {
  const args = { _: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const part = argv[i];
    if (part.startsWith('--')) {
      args[part.slice(2)] = argv[i + 1];
      i += 1;
    } else {
      args._.push(part);
    }
  }
  return args;
}

async function readPluginManifest(pluginDir) {
  const manifestPath = path.join(pluginDir, 'manifest.json');
  if (!fs.existsSync(manifestPath)) {
    throw new ManifestError('E_MANIFEST_INVALID', `缺少插件清单 ${manifestPath}`);
  }
  return JSON.parse(await readFile(manifestPath, 'utf8'));
}

function resolvePluginDir(args) {
  if (args.dir) {
    const dir = path.resolve(args.dir);
    if (!fs.existsSync(dir)) {
      throw new ManifestError('E_MANIFEST_INVALID', `插件目录不存在：${dir}`);
    }
    return dir;
  }
  if (args.pluginId) {
    const dir = path.join(pluginsRoot, args.pluginId);
    if (!fs.existsSync(dir)) {
      throw new ManifestError('E_MANIFEST_INVALID', `插件目录不存在：code/plugins/${args.pluginId}`);
    }
    return dir;
  }
  throw new ManifestError('E_MANIFEST_INVALID', 'Missing --pluginId or --dir');
}

/** lock：计算 vendor 树哈希 → 写锁文件 + 回填 manifest libraries[].hash */
async function commandLock(args) {
  const pluginDir = resolvePluginDir(args);
  const manifestPath = path.join(pluginDir, 'manifest.json');
  const manifest = await readPluginManifest(pluginDir);
  const { libraries } = validateManifestExtensions(manifest, { allowMissingHash: true });
  if (libraries.length === 0) {
    console.log('[spark-plugin-cli] manifest 无 libraries，无需锁定');
    return;
  }
  const locked = [];
  let manifestChanged = false;
  for (const library of libraries) {
    const vendorDir = path.join(pluginDir, ...vendorPathForRepo(library.repo).split('/'));
    const { hash, files } = await hashVendorTree(vendorDir);
    locked.push({
      repo: library.repo,
      commit: library.commit,
      hash,
      vendorPath: vendorPathForRepo(library.repo),
      files
    });
    if (library.hash !== hash) {
      manifestChanged = true;
    }
    console.log(`[spark-plugin-cli] locked ${library.repo}@${library.commit} sha256:${hash} (${files} files)`);
  }
  await writeLock(pluginDir, locked);
  if (manifestChanged) {
    const byRepo = new Map(locked.map((entry) => [entry.repo, entry]));
    for (const entry of manifest.libraries) {
      const match = byRepo.get(normalizeRepoId(entry.repo));
      if (match) {
        entry.hash = match.hash;
      }
    }
    await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + '\n', 'utf8');
    console.log('[spark-plugin-cli] manifest.json libraries[].hash 已回填');
  }
  console.log(`[spark-plugin-cli] ${pluginDir}/spark-libraries.lock.json 已写入`);
}

/** 构建前公共校验：manifest 扩展字段 + 锁文件 + vendor 完整性；返回 { manifest, model, lockInfo } */
async function preflight(pluginDir) {
  const manifest = await readPluginManifest(pluginDir);
  const model = validateManifestExtensions(manifest);
  let lockInfo = new Map();
  if (model.libraries.length > 0) {
    const lock = await readLock(pluginDir);
    lockInfo = await verifyLock(pluginDir, model.libraries, lock);
  }
  return { manifest, model, lockInfo };
}

/** build --mode app：SBOM + 调起 .spkg 打包（含锚定签名材料） */
async function buildApp(args, pluginDir, manifest, model, lockInfo) {
  if (model.kind !== 'app') {
    throw new ManifestError('E_KIND_MISMATCH', `manifest kind 为 library，请使用 --mode library`);
  }
  // fail-closed：安装包 = dist 全量打进，dist 残留对 vendor/ 或包外路径的引用即拒
  await assertDistSelfContained(path.join(pluginDir, 'dist'));
  const pluginId = manifest.id || path.basename(pluginDir);
  const version = (args.version ?? manifest.version ?? '').replace(/^v/, '');
  if (!version) {
    throw new ManifestError('E_MANIFEST_INVALID', `无法确定版本：--version 未传且 manifest.json 缺少 version`);
  }
  const outputDir = args.outputDir
    ? path.resolve(args.outputDir)
    : path.join(codeRoot, 'app', 'dist-market', 'plugins', pluginId);
  await mkdir(outputDir, { recursive: true });

  const sbom = buildSbom({ pluginId, version, toolVersion, libraries: model.libraries, lockInfo });
  const sbomPath = path.join(outputDir, SBOM_FILE_NAME);
  await writeFile(sbomPath, renderSbom(sbom), 'utf8');

  const packagerArgs = [
    packagerScript,
    '--pluginDir', pluginDir,
    '--pluginId', pluginId,
    '--sbom', sbomPath,
    '--outputDir', path.relative(codeRoot, outputDir) || outputDir
  ];
  if (args.version) {
    packagerArgs.push('--version', args.version);
  }
  if (args.repository) {
    packagerArgs.push('--repository', args.repository);
  }
  if (args.releaseTag) {
    packagerArgs.push('--releaseTag', args.releaseTag);
  }
  const result = spawnSync(process.execPath, packagerArgs, { stdio: 'inherit' });
  if (result.status !== 0) {
    throw new Error(`E_PACKAGE_FAILED: build-plugin-package.mjs exited with ${result.status}`);
  }
  console.log(`[spark-plugin-cli] 安装包构建完成（SBOM ${model.libraries.length} 条依赖）：${outputDir}`);
}

/** build --mode library：npm/git 引用形态的库包目录 */
async function buildLibrary(args, pluginDir, manifest, model, lockInfo) {
  if (model.kind !== 'library') {
    throw new ManifestError('E_KIND_MISMATCH', `manifest kind 非 library（缺省 app），库包构建请在 manifest.json 置 "kind": "library"`);
  }
  const pluginId = manifest.id || path.basename(pluginDir);
  const outputDir = args.outputDir
    ? path.resolve(args.outputDir)
    : path.join(codeRoot, 'app', 'dist-market', 'libraries', pluginId);
  await mkdir(outputDir, { recursive: true });
  const sbom = buildSbom({
    pluginId,
    version: manifest.version,
    toolVersion,
    libraries: model.libraries,
    lockInfo
  });
  const { packageDir, checksumsPath, files } = await buildLibraryPackage({
    pluginDir,
    manifest,
    sbom,
    outputDir
  });
  console.log(`[spark-plugin-cli] 库包构建完成（${files.length} 个文件）：${packageDir}`);
  console.log(`[spark-plugin-cli] 校验清单：${checksumsPath}`);
  console.log('[spark-plugin-cli] 发布形态：npm publish 该目录，或以 git 仓库 release/tag 引用（Spark 不自建包仓库）');
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const command = args._[0];
  if (!command || command === 'help' || args.help) {
    console.log('用法：spark-plugin-cli <lock|build|verify> [--pluginId <id> | --dir <path>] [--mode app|library] [...]');
    return;
  }
  if (command === 'lock') {
    await commandLock(args);
    return;
  }
  if (command === 'verify') {
    const pluginDir = resolvePluginDir(args);
    const { model } = await preflight(pluginDir);
    console.log(`[spark-plugin-cli] verify OK：kind=${model.kind}，libraries=${model.libraries.length} 条（锁文件与 vendor 树哈希一致）`);
    return;
  }
  if (command === 'build') {
    const pluginDir = resolvePluginDir(args);
    const { manifest, model, lockInfo } = await preflight(pluginDir);
    const mode = args.mode ?? 'app';
    if (mode === 'app') {
      await buildApp(args, pluginDir, manifest, model, lockInfo);
    } else if (mode === 'library') {
      await buildLibrary(args, pluginDir, manifest, model, lockInfo);
    } else {
      throw new ManifestError('E_MODE_INVALID', `--mode must be app or library, got ${JSON.stringify(mode)}`);
    }
    return;
  }
  throw new ManifestError('E_COMMAND_INVALID', `unknown command: ${command}（lock|build|verify）`);
}

export { main };

export function runCli() {
  main().catch((error) => {
    console.error('[spark-plugin-cli] failed:', error.message ?? error);
    process.exit(1);
  });
}
