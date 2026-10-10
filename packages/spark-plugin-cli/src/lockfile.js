/**
 * 依赖哈希锁定：vendor 树哈希 + spark-libraries.lock.json 读写与核验（plugin-dist §9.2）。
 *
 * 约定：库源码以 vendor 目录形式置于插件工程内 vendor/<规范化 repo id>/ 下
 * （由作者按 repo+commit 锚定拉取；本工具不做网络抓取，只核验内容哈希）。
 *
 * vendor 树哈希（字节级，与 plugin-dist §9.2 一致）：
 * 1. 递归收集全部文件，相对路径统一 / 分隔，按码位升序排序；
 * 2. 每个文件 sha256（hex 小写）；
 * 3. 逐行拼接 `<fileSha256>  <relPath>\n`，整体再做 sha256 即树哈希；
 *    空目录 = sha256(空串)。
 */

import { createHash } from 'crypto';
import fs from 'fs';
import { readdir, readFile, writeFile } from 'fs/promises';
import path from 'path';
import { ManifestError } from './manifest.js';

export const LOCK_FILE_NAME = 'spark-libraries.lock.json';
export const LOCK_VERSION = 1;

export class LockError extends Error {
  constructor(code, detail) {
    super(`${code}: ${detail}`);
    this.code = code;
  }
}

export function sha256Hex(content) {
  return createHash('sha256').update(content).digest('hex');
}

/** 库在插件工程内的 vendor 相对路径（posix 风格，跨平台稳定） */
export function vendorPathForRepo(repo) {
  return `vendor/${repo}`;
}

/** 码位（Unicode code point）升序比较器：与文档「按码位升序」口径一致——
 *  默认 Array.sort 是 UTF-16 码元序，含非 BMP 字符的路径两者序不同，
 *  跨实现复算会分歧（A33 评审 U4） */
export function compareCodePoints(a, b) {
  const ai = Array.from(a);
  const bi = Array.from(b);
  const n = Math.min(ai.length, bi.length);
  for (let i = 0; i < n; i += 1) {
    const ac = ai[i].codePointAt(0);
    const bc = bi[i].codePointAt(0);
    if (ac !== bc) {
      return ac - bc;
    }
  }
  return ai.length - bi.length;
}

async function walkFiles(rootDir) {
  const walk = async (dir) => {
    const entries = await readdir(dir, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      // vendor 内含符号链接即拒（A33 评审 U4）：symlink 不参与哈希会让
      // 哈希覆盖不完整且无任何提示，fail-closed 优于静默跳过
      if (entry.isSymbolicLink()) {
        throw new LockError(
          'E_VENDOR_SYMLINK',
          `vendor tree contains symbolic link: ${path.relative(rootDir, fullPath)}（vendor 必须为实体文件树）`
        );
      }
      if (entry.isDirectory()) {
        files.push(...(await walk(fullPath)));
      } else if (entry.isFile()) {
        files.push(path.relative(rootDir, fullPath).split(path.sep).join('/'));
      }
    }
    return files;
  };
  return (await walk(rootDir)).sort(compareCodePoints);
}

/** 计算 vendor 树哈希；返回 { hash, files }（files 为参与计数的文件数） */
export async function hashVendorTree(dir) {
  if (!fs.existsSync(dir)) {
    throw new LockError('E_VENDOR_MISSING', `vendor dir missing: ${dir}`);
  }
  const files = await walkFiles(dir);
  const lines = [];
  for (const relPath of files) {
    const content = await readFile(path.join(dir, relPath));
    lines.push(`${sha256Hex(content)}  ${relPath}\n`);
  }
  return { hash: sha256Hex(lines.join('')), files: files.length };
}

export function lockPathForPlugin(pluginDir) {
  return path.join(pluginDir, LOCK_FILE_NAME);
}

export async function readLock(pluginDir) {
  const lockPath = lockPathForPlugin(pluginDir);
  if (!fs.existsSync(lockPath)) {
    throw new LockError(
      'E_LOCK_MISSING',
      `${LOCK_FILE_NAME} missing in ${pluginDir} (run: spark-plugin-cli lock)`
    );
  }
  let lock;
  try {
    lock = JSON.parse(await readFile(lockPath, 'utf8'));
  } catch (error) {
    throw new LockError('E_LOCK_INVALID', `${LOCK_FILE_NAME} is not valid JSON: ${error.message}`);
  }
  if (lock.lockfileVersion !== LOCK_VERSION || !Array.isArray(lock.libraries)) {
    throw new LockError('E_LOCK_INVALID', `${LOCK_FILE_NAME}: unsupported shape (lockfileVersion/libraries)`);
  }
  return lock;
}

export async function writeLock(pluginDir, libraries) {
  // 幂等写（A33 评审 S1）：libraries 内容不变时保留既有 generatedAt，
  // 重复 lock 不产生无谓 diff；仅在依赖集变化时刷新时间戳
  let generatedAt = new Date().toISOString();
  const lockPath = lockPathForPlugin(pluginDir);
  if (fs.existsSync(lockPath)) {
    try {
      const existing = JSON.parse(await readFile(lockPath, 'utf8'));
      if (JSON.stringify(existing?.libraries) === JSON.stringify(libraries) && typeof existing?.generatedAt === 'string') {
        generatedAt = existing.generatedAt;
      }
    } catch {
      // 既有锁文件不可解析：按全新写入处理
    }
  }
  const lock = {
    lockfileVersion: LOCK_VERSION,
    generatedAt,
    libraries
  };
  await writeFile(lockPath, JSON.stringify(lock, null, 2) + '\n', 'utf8');
  return lock;
}

/**
 * 构建期核验（integrity 不符必拒，逐条对齐 runtime-and-trust §4.1-2/§六）：
 * 1. manifest.libraries 与锁文件按 repo+commit+hash 逐条相等（集合一致）；
 * 2. 重算每个 vendor 树哈希，必须等于声明 hash。
 * 返回 Map<repo, { files }>（供 SBOM 使用）。
 */
export async function verifyLock(pluginDir, libraries, lock) {
  const locked = new Map(lock.libraries.map((entry) => [entry.repo, entry]));
  const result = new Map();
  for (const library of libraries) {
    const entry = locked.get(library.repo);
    if (!entry) {
      throw new LockError('E_LOCK_MISMATCH', `library not in lockfile: ${library.repo}`);
    }
    if (entry.commit !== library.commit) {
      throw new LockError(
        'E_LOCK_MISMATCH',
        `commit mismatch for ${library.repo}: manifest ${library.commit} vs lock ${entry.commit}`
      );
    }
    if (entry.hash !== library.hash) {
      throw new LockError(
        'E_LOCK_MISMATCH',
        `hash mismatch for ${library.repo}: manifest ${library.hash} vs lock ${entry.hash}`
      );
    }
    const vendorDir = path.join(pluginDir, ...vendorPathForRepo(library.repo).split('/'));
    const { hash, files } = await hashVendorTree(vendorDir);
    if (hash !== library.hash) {
      throw new LockError(
        'E_LIB_INTEGRITY',
        `vendor tree integrity mismatch for ${library.repo}: declared ${library.hash}, actual ${hash}`
      );
    }
    result.set(library.repo, { files });
    locked.delete(library.repo);
  }
  for (const repo of locked.keys()) {
    throw new LockError('E_LOCK_MISMATCH', `lockfile entry not declared in manifest: ${repo}`);
  }
  return result;
}
