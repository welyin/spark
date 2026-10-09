#!/usr/bin/env node

/**
 * manifest kind / libraries 字段的读取与校验（plugin-dist §9.1）。
 *
 * 拍板口径（2026-10-09 decisions 档二-3 / runtime-and-trust §4.1）：
 * - kind: "app" | "library"，缺省 app；library = 纯代码库，不单独运行、无数据域；
 * - libraries: [{ repo, commit, hash }]，依赖以仓库地址 + commit 锚定 + sha256 内容
 *   哈希为准，不以包注册中心名为准（名字可抢注，URL 不可抢注）；
 * - commit 只接受 40 位小写 hex（精确提交），分支/标签等浮动引用一律拒绝；
 * - hash 为 64 位小写 hex（vendor 树 sha256，算法见 lockfile.js / plugin-dist §9.2）。
 *
 * repo 语法与规范化与 plugin-dist §1.1/§1.2（插件 id）逐条对齐。
 */

const REPO_HOSTS = new Set(['github.com', 'gitlab.com', 'gitee.com']);
const SEGMENT_RE = /^[a-z0-9._-]+$/;
const COMMIT_RE = /^[0-9a-f]{40}$/;
const HASH_RE = /^[0-9a-f]{64}$/;

export class ManifestError extends Error {
  constructor(code, detail) {
    super(`${code}: ${detail}`);
    this.code = code;
  }
}

/**
 * 规范化仓库地址（plugin-dist §1.2 同序）：
 * 1. 去首尾空白；剥掉一次 https?:// 前缀（大小写不敏感）；
 * 2. 去末尾 /（重复至无尾斜杠）；repo 段去一次 .git 后缀；
 * 3. 全串转小写；
 * 4. 按 §1.1 校验，失败即抛 E_LIB_REPO。
 */
export function normalizeRepoId(input) {
  if (typeof input !== 'string' || input.trim() === '') {
    throw new ManifestError('E_LIB_REPO', `repo must be a non-empty string, got ${JSON.stringify(input)}`);
  }
  let text = input.trim();
  text = text.replace(/^https?:\/\//i, '');
  while (text.endsWith('/')) {
    text = text.slice(0, -1);
  }
  text = text.toLowerCase();
  const segments = text.split('/');
  if (segments.length >= 2 && segments[1].length > 0) {
    // repo 段（第 3 段）去 .git 后缀（一次）
    const repoIndex = 2;
    if (segments[repoIndex] && segments[repoIndex].endsWith('.git')) {
      segments[repoIndex] = segments[repoIndex].slice(0, -4);
    }
  }
  const normalized = segments.join('/');
  validateRepoId(normalized, input);
  return normalized;
}

function validateRepoId(id, originalInput) {
  const fail = (reason) => {
    throw new ManifestError('E_LIB_REPO', `repo invalid: ${JSON.stringify(originalInput)}: ${reason}`);
  };
  if (Buffer.byteLength(id, 'utf8') > 256) {
    fail('id longer than 256 bytes');
  }
  const segments = id.split('/');
  if (segments.length < 3) {
    fail('expected host/owner/repo[/sub-path]');
  }
  if (!REPO_HOSTS.has(segments[0])) {
    fail(`host must be one of ${[...REPO_HOSTS].join('/')}, got ${segments[0] || '(empty)'}`);
  }
  const [host, owner, repo, ...subPath] = segments;
  for (const [label, segment, max] of [['owner', owner, 100], ['repo', repo, 100]]) {
    if (!segment || segment.length > max || !SEGMENT_RE.test(segment)) {
      fail(`${label} segment invalid: ${JSON.stringify(segment)}`);
    }
  }
  if (subPath.length > 8) {
    fail('sub-path deeper than 8 segments');
  }
  for (const segment of [...subPath]) {
    if (!segment || segment.length > 64 || !SEGMENT_RE.test(segment)) {
      fail(`sub-path segment invalid: ${JSON.stringify(segment)}`);
    }
  }
  for (const segment of segments) {
    if (segment === '.' || segment === '..') {
      fail('segment must not be . or ..');
    }
    if (/[\\%\s?#]/.test(segment)) {
      fail(`segment contains forbidden character: ${JSON.stringify(segment)}`);
    }
  }
}

export function isValidCommit(value) {
  return typeof value === 'string' && COMMIT_RE.test(value);
}

export function isValidHash(value) {
  return typeof value === 'string' && HASH_RE.test(value);
}

/**
 * 校验并规范化一条 libraries 条目。
 * allowMissingHash：lock 命令专用——作者先写 repo+commit，lock 计算后回填 hash；
 * build/verify 一律严格（hash 必填且格式正确）。
 */
export function validateLibraryEntry(entry, index, { allowMissingHash = false } = {}) {
  const where = `libraries[${index}]`;
  if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
    throw new ManifestError('E_LIBRARIES_TYPE', `${where} must be an object { repo, commit, hash }`);
  }
  const repo = normalizeRepoId(entry.repo);
  if (typeof entry.commit !== 'string' || entry.commit === '') {
    throw new ManifestError('E_LIB_COMMIT', `${where}.commit is required (40-hex pinned commit)`);
  }
  if (!isValidCommit(entry.commit)) {
    throw new ManifestError(
      'E_LIB_COMMIT_FLOAT',
      `${where}.commit must be a 40-hex pinned commit, floating ref rejected: ${JSON.stringify(entry.commit)}`
    );
  }
  if (entry.hash === undefined || entry.hash === null || entry.hash === '') {
    if (!allowMissingHash) {
      throw new ManifestError('E_LIB_HASH', `${where}.hash is required (run: spark-plugin-cli lock)`);
    }
    return { repo, commit: entry.commit, hash: '' };
  }
  if (!isValidHash(entry.hash)) {
    throw new ManifestError(
      'E_LIB_HASH',
      `${where}.hash must be a 64-hex sha256, got ${JSON.stringify(entry.hash)}`
    );
  }
  return { repo, commit: entry.commit, hash: entry.hash };
}

/**
 * 读取并校验 manifest 的 kind / libraries 扩展字段（其余字段不在本模块职责内）。
 * 返回 { kind, libraries }（libraries 已规范化、去重校验后）。
 */
export function validateManifestExtensions(manifest, { allowMissingHash = false } = {}) {
  if (typeof manifest !== 'object' || manifest === null) {
    throw new ManifestError('E_MANIFEST_INVALID', 'manifest must be a JSON object');
  }
  const kind = manifest.kind ?? 'app';
  if (kind !== 'app' && kind !== 'library') {
    throw new ManifestError('E_MANIFEST_KIND', `kind must be "app" or "library", got ${JSON.stringify(manifest.kind)}`);
  }
  const rawLibraries = manifest.libraries ?? [];
  if (!Array.isArray(rawLibraries)) {
    throw new ManifestError('E_LIBRARIES_TYPE', 'libraries must be an array of { repo, commit, hash }');
  }
  const libraries = rawLibraries.map((entry, index) =>
    validateLibraryEntry(entry, index, { allowMissingHash })
  );
  const seen = new Set();
  for (const library of libraries) {
    if (seen.has(library.repo)) {
      throw new ManifestError('E_LIB_DUP', `duplicate library repo: ${library.repo}`);
    }
    seen.add(library.repo);
  }
  return { kind, libraries };
}
