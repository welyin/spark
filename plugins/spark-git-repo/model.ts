/**
 * 代码仓库应用（spark-git-repo）· 领域模型（纯函数，不依赖 SDK 运行时 / Vue）。
 *
 * 依据 git-repo.md v0.2 §3 数据模型与拍板口径：
 * - 镜像清单 = 项目议题内的签名事务操作（档二-5，payload.kind = 'git.mirror.manifest'），
 *   逐 Git 对象散 blob（档三-6）——blob 内容为 git 对象的**原始（已解压）字节**，
 *   cid = SHA-256（内容面既有口径）；浏览侧纯 JS 解析，无需 isomorphic-git；
 * - PR = 子事务 + pr.open/update/comment/review/merged/closed 操作集（§3.2）；
 * - 合并 = 单维护者合并回执即生效（档一-3）；写回禁止非快进（档一-3/O7）——
 *   FF 判定与「回执 commit ∈ 新镜像历史」核验均为纯函数，任何节点可独立重算。
 */

import type { AffairLogEntry } from '../../packages/plugin-sdk/src';

// ------------------------------------------------------------------
// 常量
// ------------------------------------------------------------------

/** PR 子事务类型标识（affairTypes 只生产不注册，X8：注册归「项目」插件） */
export const PR_AFFAIR_TYPE = 'spark-git-repo:pr';

/** 镜像清单操作 kind（项目议题日志内） */
export const MIRROR_MANIFEST_KIND = 'git.mirror.manifest';

/** PR 操作 kind 集（PR 子事务日志内） */
export const PR_OP_KINDS = ['pr.open', 'pr.update', 'pr.comment', 'pr.review', 'pr.merged', 'pr.closed'] as const;
export type PrOpKind = (typeof PR_OP_KINDS)[number];

/** 附件体积提示阈值（档三-7：10MB 提示 + 拆分建议，硬上限归内核体积卫生） */
export const ATTACHMENT_WARN_BYTES = 10 * 1024 * 1024;

/** 浏览历史单灾遍历上限（大仓库体验排后续，档三-6） */
export const HISTORY_WALK_CAP = 5000;

// ------------------------------------------------------------------
// base64（浏览器/jsdom 安全，分块避免栈溢出）
// ------------------------------------------------------------------

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export function utf8ToBytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

export function bytesToUtf8(bytes: Uint8Array): string {
  return new TextDecoder('utf-8').decode(bytes);
}

// ------------------------------------------------------------------
// Git 对象解析（原始字节 = git cat-file 输出；tree 为二进制线形）
// ------------------------------------------------------------------

export type GitObjectType = 'commit' | 'tree' | 'blob' | 'tag';

export type GitObject = { type: GitObjectType; bytes: Uint8Array };

/** 浏览侧对象仓库抽象（service 以内容面 blob 物化为内存 Map 后适配） */
export type GitObjectStore = { get: (sha: string) => GitObject | null };

export function isGitSha(value: unknown): value is string {
  return typeof value === 'string' && (/^[0-9a-f]{40}$/.test(value) || /^[0-9a-f]{64}$/.test(value));
}

function toHex(bytes: Uint8Array): string {
  let hex = '';
  for (const byte of bytes) {
    hex += byte.toString(16).padStart(2, '0');
  }
  return hex;
}

export type ParsedCommit = {
  sha: string;
  tree: string;
  parents: string[];
  author: string;
  authorTimeMs: number | null;
  committerTimeMs: number | null;
  message: string;
  /** 首行（列表展示用） */
  subject: string;
};

/** 解析 commit 对象（头部键值 + 续行折叠 + 空行后 message；签名头等未知键跳过） */
export function parseCommit(sha: string, bytes: Uint8Array): ParsedCommit | null {
  const text = bytesToUtf8(bytes);
  const blank = text.indexOf('\n\n');
  if (blank === -1) {
    return null;
  }
  const headerText = text.slice(0, blank);
  const message = text.slice(blank + 2).replace(/\n$/, '');
  const headers = new Map<string, string[]>();
  let lastKey: string | null = null;
  for (const line of headerText.split('\n')) {
    if (line.startsWith(' ') && lastKey !== null) {
      const list = headers.get(lastKey);
      if (list && list.length > 0) {
        list[list.length - 1] += `\n${line.slice(1)}`;
      }
      continue;
    }
    const space = line.indexOf(' ');
    if (space === -1) {
      continue;
    }
    const key = line.slice(0, space);
    const value = line.slice(space + 1);
    const list = headers.get(key) ?? [];
    list.push(value);
    headers.set(key, list);
    lastKey = key;
  }
  const tree = headers.get('tree')?.[0];
  if (!tree) {
    return null;
  }
  const timeOf = (line: string | undefined): number | null => {
    const match = line?.match(/(\d+)\s+[+-]\d{4}$/);
    return match ? Number(match[1]) * 1000 : null;
  };
  const authorLine = headers.get('author')?.[0] ?? '';
  return {
    sha,
    tree,
    parents: headers.get('parent') ?? [],
    author: authorLine.replace(/\s*<[^>]*>\s*\d+\s+[+-]\d{4}$/, ''),
    authorTimeMs: timeOf(authorLine),
    committerTimeMs: timeOf(headers.get('committer')?.[0]),
    message,
    subject: message.split('\n')[0] ?? ''
  };
}

export type TreeEntry = {
  /** git 模式串（如 '100644' / '100755' / '040000' / '120000'） */
  mode: string;
  name: string;
  sha: string;
  type: 'tree' | 'blob' | 'commit';
};

/** 解析 tree 对象（二进制线形：`<mode> <name>\0<raw sha>` 重复；MVP 仅支持 SHA-1 仓库，hashLen 固定 20） */
export function parseTree(bytes: Uint8Array, hashLen = 20): TreeEntry[] {
  const entries: TreeEntry[] = [];
  let offset = 0;
  while (offset < bytes.length) {
    const space = bytes.indexOf(0x20, offset);
    if (space === -1) {
      break;
    }
    const mode = bytesToUtf8(bytes.subarray(offset, space));
    const nul = bytes.indexOf(0, space + 1);
    if (nul === -1 || nul + 1 + hashLen > bytes.length) {
      break;
    }
    const name = bytesToUtf8(bytes.subarray(space + 1, nul));
    const sha = toHex(bytes.subarray(nul + 1, nul + 1 + hashLen));
    entries.push({
      mode,
      name,
      sha,
      // git tree 线形中目录模式为 '40000'（无前导零），兼容零填充形态
      type: mode === '40000' || mode === '040000' ? 'tree' : mode === '160000' ? 'commit' : 'blob'
    });
    offset = nul + 1 + hashLen;
  }
  return entries;
}

/** 按路径在树中定位条目（逐段下钻；任一段缺失返回 null） */
export function lookupPath(
  store: GitObjectStore,
  rootTreeSha: string,
  path: string
): TreeEntry | null {
  const segments = path.split('/').filter(Boolean);
  let current: TreeEntry = { mode: '040000', name: '', sha: rootTreeSha, type: 'tree' };
  for (const segment of segments) {
    if (current.type !== 'tree') {
      return null;
    }
    const tree = store.get(current.sha);
    if (!tree || tree.type !== 'tree') {
      return null;
    }
    const entry = parseTree(tree.bytes).find((item) => item.name === segment) ?? null;
    if (!entry) {
      return null;
    }
    current = entry;
  }
  return current;
}

// ------------------------------------------------------------------
// 提交历史与可达性（FF 判定 / 合并回执核验的公共内核）
// ------------------------------------------------------------------

/** 读取一个 commit（对象缺失或形状非法返回 null） */
export function readCommit(store: GitObjectStore, sha: string): ParsedCommit | null {
  const obj = store.get(sha);
  if (!obj || obj.type !== 'commit') {
    return null;
  }
  return parseCommit(sha, obj.bytes);
}

/**
 * 提交历史（自 head 可达的全部 commit，按 committer 时刻降序 + sha tie-break）。
 * 遍历设 HISTORY_WALK_CAP 上限（大仓库排后续）；对象缺失（镜像未收敛）时跳过
 * 该支并继续——调用方凭 mirrorStatus 如实呈现，不伪造连续历史。
 */
export function listCommits(store: GitObjectStore, headSha: string, limit = 200): ParsedCommit[] {
  const seen = new Map<string, ParsedCommit>();
  const queue = [headSha];
  while (queue.length > 0 && seen.size < HISTORY_WALK_CAP) {
    const sha = queue.shift() as string;
    if (seen.has(sha)) {
      continue;
    }
    const commit = readCommit(store, sha);
    if (!commit) {
      continue;
    }
    seen.set(sha, commit);
    for (const parent of commit.parents) {
      if (!seen.has(parent)) {
        queue.push(parent);
      }
    }
  }
  return [...seen.values()]
    .sort((a, b) => (b.committerTimeMs ?? 0) - (a.committerTimeMs ?? 0) || (a.sha < b.sha ? -1 : 1))
    .slice(0, Math.max(1, limit));
}

/**
 * ancestor 是否为 descendant 的祖先（含自身）——禁止非快进写回（档一-3/O7）
 * 与「回执 commit ∈ 新镜像历史」（§3.4）共用的可达性判定。
 */
export function isAncestor(store: GitObjectStore, ancestor: string, descendant: string): boolean {
  if (ancestor === descendant) {
    return true;
  }
  const seen = new Set<string>();
  const queue = [descendant];
  while (queue.length > 0 && seen.size < HISTORY_WALK_CAP * 4) {
    const sha = queue.shift() as string;
    if (seen.has(sha)) {
      continue;
    }
    seen.add(sha);
    const commit = readCommit(store, sha);
    if (!commit) {
      continue;
    }
    for (const parent of commit.parents) {
      if (parent === ancestor) {
        return true;
      }
      if (!seen.has(parent)) {
        queue.push(parent);
      }
    }
  }
  return false;
}

// ------------------------------------------------------------------
// 行级 diff（Myers）与树间 diff
// ------------------------------------------------------------------

export type DiffLine = { type: 'context' | 'add' | 'del'; text: string };

/** diff 视图输入体积护栏（两侧文本合计字符数上限）：Myers 复杂度 O((n+m)·D) 且留存全部轨迹，超限如实标注「diff 过大不展开」 */
export const DIFF_TEXT_CAP_CHARS = 256 * 1024;

/** diff 输入是否超出体积护栏（diffLines 调用前置判定） */
export function diffOversized(oldText: string, newText: string): boolean {
  return oldText.length + newText.length > DIFF_TEXT_CAP_CHARS;
}

/** Myers 行 diff（全量操作列；大文件调用方应先做体积护栏） */
export function diffLines(oldText: string, newText: string): DiffLine[] {
  const a = oldText === '' ? [] : oldText.split('\n');
  const b = newText === '' ? [] : newText.split('\n');
  const n = a.length;
  const m = b.length;
  const max = n + m;
  if (max === 0) {
    return [];
  }
  // V 数组轨迹（k → 该 D 下最远 x）
  const trace: Array<Map<number, number>> = [];
  let v = new Map<number, number>([[1, 0]]);
  let foundD = -1;
  outer: for (let d = 0; d <= max; d++) {
    trace.push(new Map(v));
    const next = new Map<number, number>();
    for (let k = -d; k <= d; k += 2) {
      let x: number;
      if (k === -d || (k !== d && (v.get(k - 1) ?? -1) < (v.get(k + 1) ?? -1))) {
        x = v.get(k + 1) ?? 0;
      } else {
        x = (v.get(k - 1) ?? 0) + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      next.set(k, x);
      if (x >= n && y >= m) {
        trace.push(next);
        foundD = d;
        break outer;
      }
    }
    v = next;
  }
  if (foundD === -1) {
    return [];
  }
  // 回溯生成操作列
  const ops: DiffLine[] = [];
  let x = n;
  let y = m;
  for (let d = foundD; d > 0; d--) {
    const prev = trace[d];
    const k = x - y;
    let prevK: number;
    if (k === -d || (k !== d && (prev.get(k - 1) ?? -1) < (prev.get(k + 1) ?? -1))) {
      prevK = k + 1;
    } else {
      prevK = k - 1;
    }
    const prevX = prev.get(prevK) ?? 0;
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      ops.push({ type: 'context', text: a[x - 1] });
      x -= 1;
      y -= 1;
    }
    if (x > prevX) {
      ops.push({ type: 'del', text: a[x - 1] });
      x -= 1;
    } else if (y > prevY) {
      ops.push({ type: 'add', text: b[y - 1] });
      y -= 1;
    }
  }
  while (x > 0 && y > 0) {
    ops.push({ type: 'context', text: a[x - 1] });
    x -= 1;
    y -= 1;
  }
  return ops.reverse();
}

/** 折叠长上下文段为省略标记（diff 视图展示用；head/tail 各保留 ctx 行） */
export type DisplayDiffRow = DiffLine | { type: 'gap'; count: number };

export function collapseContext(ops: DiffLine[], ctx = 3): DisplayDiffRow[] {
  const rows: DisplayDiffRow[] = [];
  let run: DiffLine[] = [];
  const flush = (keepHead: number, keepTail: number): void => {
    if (run.length <= keepHead + keepTail + 1) {
      rows.push(...run);
    } else {
      rows.push(...run.slice(0, keepHead));
      rows.push({ type: 'gap', count: run.length - keepHead - keepTail });
      rows.push(...run.slice(run.length - keepTail));
    }
    run = [];
  };
  for (const op of ops) {
    if (op.type === 'context') {
      run.push(op);
    } else {
      if (run.length > 0) {
        flush(ctx, ctx);
      }
      rows.push(op);
    }
  }
  if (run.length > 0) {
    // 结尾长上下文：只保留头部（尾部无变更行可锚）
    if (run.length > ctx * 2 + 1) {
      rows.push(...run.slice(0, ctx));
      rows.push({ type: 'gap', count: run.length - ctx });
    } else {
      rows.push(...run);
    }
  }
  return rows;
}

export type FileChange = {
  path: string;
  change: 'added' | 'removed' | 'modified';
  oldSha?: string;
  newSha?: string;
};

/** 两棵树递归 diff（symlink/submodule 按 blob/commit 条目比 sha，不展开内容） */
export function diffTrees(store: GitObjectStore, oldTreeSha: string | null, newTreeSha: string | null, prefix = ''): FileChange[] {
  const readEntries = (sha: string | null): TreeEntry[] => {
    if (!sha) {
      return [];
    }
    const obj = store.get(sha);
    return obj && obj.type === 'tree' ? parseTree(obj.bytes) : [];
  };
  const oldEntries = new Map(readEntries(oldTreeSha).map((entry) => [entry.name, entry]));
  const newEntries = new Map(readEntries(newTreeSha).map((entry) => [entry.name, entry]));
  const changes: FileChange[] = [];
  const names = [...new Set([...oldEntries.keys(), ...newEntries.keys()])].sort();
  for (const name of names) {
    const before = oldEntries.get(name) ?? null;
    const after = newEntries.get(name) ?? null;
    const path = prefix ? `${prefix}/${name}` : name;
    if (before && after && before.sha === after.sha && before.type === after.type) {
      continue;
    }
    if (before?.type === 'tree' || after?.type === 'tree') {
      if ((before?.type ?? 'tree') === 'tree' && (after?.type ?? 'tree') === 'tree') {
        changes.push(...diffTrees(store, before?.sha ?? null, after?.sha ?? null, path));
        continue;
      }
      // 树 ↔ 非树类型翻转：整棵移除 + 新条目（如实呈现，不逐文件编造）
      if (before?.type === 'tree') {
        for (const leaf of flattenTree(store, before.sha, path)) {
          changes.push({ path: leaf.path, change: 'removed', oldSha: leaf.sha });
        }
      }
      if (after?.type === 'tree') {
        for (const leaf of flattenTree(store, after.sha, path)) {
          changes.push({ path: leaf.path, change: 'added', newSha: leaf.sha });
        }
      }
      if (before && before.type !== 'tree' && !after) {
        changes.push({ path, change: 'removed', oldSha: before.sha });
      }
      if (after && after.type !== 'tree' && !before) {
        changes.push({ path, change: 'added', newSha: after.sha });
      }
      continue;
    }
    if (before && !after) {
      changes.push({ path, change: 'removed', oldSha: before.sha });
    } else if (!before && after) {
      changes.push({ path, change: 'added', newSha: after.sha });
    } else if (before && after) {
      changes.push({ path, change: 'modified', oldSha: before.sha, newSha: after.sha });
    }
  }
  return changes;
}

/** 展开一棵树为全部叶子条目（type 翻转降级用） */
export function flattenTree(store: GitObjectStore, treeSha: string, prefix = ''): Array<{ path: string; sha: string }> {
  const obj = store.get(treeSha);
  if (!obj || obj.type !== 'tree') {
    return [];
  }
  const out: Array<{ path: string; sha: string }> = [];
  for (const entry of parseTree(obj.bytes)) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.type === 'tree') {
      out.push(...flattenTree(store, entry.sha, path));
    } else {
      out.push({ path, sha: entry.sha });
    }
  }
  return out;
}

/** 二进制粗判（前 8KB 含 NUL 即视为二进制，diff 视图如实标注） */
export function looksBinary(bytes: Uint8Array): boolean {
  const probe = bytes.subarray(0, 8000);
  return probe.includes(0);
}

// ------------------------------------------------------------------
// 镜像清单（git.mirror.manifest 操作载荷）
// ------------------------------------------------------------------

export type MirrorObjectRef = { sha: string; type: GitObjectType; cid: string };

export type MirrorBranch = { name: string; head: string };

export type MirrorManifest = {
  repo: string;
  defaultBranch: string;
  branches: MirrorBranch[];
  /** 单调递增版本号（同号冲突取 opHash 大者，见 latestMirrorManifest） */
  version: number;
  objects: MirrorObjectRef[];
  /** 首个清单版本记录一次性导入的 HEAD commit（档一-4 创世哈希口径的账本侧落点） */
  importHead?: string;
  note?: string;
};

function isCid(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
}

/** 从操作载荷解析镜像清单（形状不符一律 null——坏操作如实跳过，不脑补） */
export function parseMirrorManifest(payload: unknown): MirrorManifest | null {
  if (typeof payload !== 'object' || payload === null || Array.isArray(payload)) {
    return null;
  }
  const raw = payload as Record<string, unknown>;
  if (raw.kind !== MIRROR_MANIFEST_KIND) {
    return null;
  }
  if (typeof raw.repo !== 'string' || raw.repo.trim() === '') {
    return null;
  }
  if (typeof raw.defaultBranch !== 'string' || raw.defaultBranch.trim() === '') {
    return null;
  }
  if (!Number.isInteger(raw.version) || (raw.version as number) < 1) {
    return null;
  }
  if (!Array.isArray(raw.branches) || raw.branches.length === 0) {
    return null;
  }
  const branches: MirrorBranch[] = [];
  for (const item of raw.branches) {
    if (typeof item !== 'object' || item === null) {
      return null;
    }
    const branch = item as Record<string, unknown>;
    if (typeof branch.name !== 'string' || branch.name.trim() === '' || !isGitSha(branch.head)) {
      return null;
    }
    branches.push({ name: branch.name, head: branch.head });
  }
  if (!Array.isArray(raw.objects)) {
    return null;
  }
  const objects: MirrorObjectRef[] = [];
  for (const item of raw.objects) {
    if (typeof item !== 'object' || item === null) {
      return null;
    }
    const ref = item as Record<string, unknown>;
    if (!isGitSha(ref.sha) || !isCid(ref.cid) || !['commit', 'tree', 'blob', 'tag'].includes(String(ref.type))) {
      return null;
    }
    objects.push({ sha: ref.sha, type: ref.type as GitObjectType, cid: ref.cid });
  }
  if (raw.importHead !== undefined && !isGitSha(raw.importHead)) {
    return null;
  }
  if (raw.note !== undefined && typeof raw.note !== 'string') {
    return null;
  }
  return {
    repo: raw.repo,
    defaultBranch: raw.defaultBranch,
    branches,
    version: raw.version as number,
    objects,
    ...(raw.importHead !== undefined ? { importHead: raw.importHead as string } : {}),
    ...(raw.note !== undefined ? { note: raw.note as string } : {})
  };
}

/**
 * 从项目议题日志取最新镜像清单：version 大者胜，同 version 取 opHash 字典序
 * 大者（§8 排序键，确定性 tie-break，串行合并纪律下不应出现同号冲突——
 * 出现时即「有维护者违背串行约定」的客观证据，UI 如实标注）。
 */
export function latestMirrorManifest(ops: AffairLogEntry[]): { manifest: MirrorManifest; opHash: string; conflicts: number } | null {
  let best: { manifest: MirrorManifest; opHash: string } | null = null;
  let conflicts = 0;
  for (const entry of ops) {
    const op = entry.op as Record<string, unknown>;
    const manifest = parseMirrorManifest(op?.payload);
    if (!manifest) {
      continue;
    }
    if (!best) {
      best = { manifest, opHash: entry.opHash };
      continue;
    }
    if (manifest.version > best.manifest.version) {
      best = { manifest, opHash: entry.opHash };
    } else if (manifest.version === best.manifest.version) {
      conflicts += 1;
      if (entry.opHash > best.opHash) {
        best = { manifest, opHash: entry.opHash };
      }
    }
  }
  return best ? { ...best, conflicts } : null;
}

/** 镜像同步状态（持有即做种的副本健康如实呈现，§4） */
export type MirrorSyncStatus = {
  total: number;
  local: number;
  missing: string[];
};

export function mirrorSyncStatus(manifest: MirrorManifest, localCids: ReadonlySet<string>): MirrorSyncStatus {
  const missing = manifest.objects.filter((ref) => !localCids.has(ref.cid)).map((ref) => ref.cid);
  return { total: manifest.objects.length, local: manifest.objects.length - missing.length, missing };
}

// ------------------------------------------------------------------
// PR 子事务状态推导（pr.open/update/comment/review/merged/closed）
// ------------------------------------------------------------------

/**
 * 项目议题规则文档 → 写权集合（维护者身份 id 列表）。
 * 字段约定（MVP 先行，git-repo.md §2「维护者 = 议题规则声明的写权集合」、
 * §3.2 关闭操作「操作者须 ∈ 写权集合，由议题规则校验」）：
 * `rules.maintainers: string[]`——64-hex 身份 id，与操作 actor.identity 同口径
 * （插件域身份 id；项目插件落地前由本约定承载）。
 * 字段缺失 / 形状非法 → 空集（fail-closed：无人的终态操作可被采纳）。
 */
export function extractWriteSet(rulesDoc: unknown): string[] {
  if (typeof rulesDoc !== 'object' || rulesDoc === null || Array.isArray(rulesDoc)) {
    return [];
  }
  const list = (rulesDoc as Record<string, unknown>).maintainers;
  if (!Array.isArray(list)) {
    return [];
  }
  return list.filter((item): item is string => typeof item === 'string' && /^[0-9a-f]{64}$/.test(item));
}

export type PrAttachmentKind = 'bundle' | 'patch-series';

export type PrAttachment = { kind: PrAttachmentKind; cid: string; size: number; name?: string };

export type PrOpenPayload = {
  title: string;
  description: string;
  base: string;
  head: string;
  attachments: PrAttachment[];
};

export type PrUpdatePayload = { head: string; attachments: PrAttachment[]; note?: string };

export type PrCommentPayload = { text: string; ref?: { path: string; line: number } };

export type PrReviewVerdict = 'approve' | 'request-changes' | 'comment';
export type PrReviewPayload = { verdict: PrReviewVerdict; text: string };

export type PrMergedPayload = { resultCommit: string; mirrorVersion: number; note?: string };

export type PrClosedPayload = { reason: string };

export type PrStatus = 'open' | 'merged' | 'closed';

export type PrTimelineItem = {
  opHash: string;
  kind: PrOpKind;
  actor: string;
  declaredAt: number;
  payload: Record<string, unknown>;
  /** 终态操作被写权校验拒绝时的如实标注（操作仍 append-only 留痕，但不改状态） */
  rejectedReason?: string;
};

export type PrState = {
  status: PrStatus;
  open: (PrOpenPayload & { actor: string; declaredAt: number; opHash: string }) | null;
  updates: Array<PrUpdatePayload & { actor: string; declaredAt: number; opHash: string }>;
  comments: Array<PrCommentPayload & { actor: string; declaredAt: number; opHash: string }>;
  reviews: Array<PrReviewPayload & { actor: string; declaredAt: number; opHash: string }>;
  merged: (PrMergedPayload & { actor: string; declaredAt: number; opHash: string }) | null;
  closed: (PrClosedPayload & { actor: string; declaredAt: number; opHash: string }) | null;
  timeline: PrTimelineItem[];
  /** 当前 head / 附件（open + 逐次 update 的 LWW 推导） */
  currentHead: string | null;
  currentAttachments: PrAttachment[];
};

function parseAttachments(value: unknown): PrAttachment[] | null {
  if (!Array.isArray(value) || value.length === 0) {
    return null;
  }
  const attachments: PrAttachment[] = [];
  for (const item of value) {
    if (typeof item !== 'object' || item === null) {
      return null;
    }
    const raw = item as Record<string, unknown>;
    if (!['bundle', 'patch-series'].includes(String(raw.kind)) || !isCid(raw.cid)) {
      return null;
    }
    if (!Number.isInteger(raw.size) || (raw.size as number) < 0) {
      return null;
    }
    attachments.push({
      kind: raw.kind as PrAttachmentKind,
      cid: raw.cid,
      size: raw.size as number,
      ...(typeof raw.name === 'string' ? { name: raw.name } : {})
    });
  }
  return attachments;
}

function actorOf(op: Record<string, unknown>): string {
  const actor = op.actor as Record<string, unknown> | undefined;
  return typeof actor?.identity === 'string' ? actor.identity : '(未知操作者)';
}

function declaredAtOf(op: Record<string, unknown>): number {
  return typeof op.declaredAt === 'number' ? op.declaredAt : 0;
}

/**
 * 从 PR 子事务日志推导 PR 状态。纪律：
 * - 首个合法 pr.open 为创建操作；其后的 pr.open 忽略（如实留在 timeline）；
 * - 首个 pr.merged / pr.closed 决定终态（单维护者回执即生效，档一-3）；
 *   终态之后的操作仍入 timeline（append-only 历史可考）但不再改状态；
 * - 写权校验（档一-3/§3.2「操作者须 ∈ 写权集合」）：提供 writeSet 时，集合外
 *   actor 的 merged/closed 不采纳为终态——操作保留在 timeline 并标注
 *   rejectedReason；writeSet 缺省 = 不做写权过滤（纯模型旧口径，服务层恒提供）；
 * - 形状非法的操作一律跳过状态推导、保留在 timeline（不脑补语义）。
 */
export function derivePrState(ops: AffairLogEntry[], writeSet?: ReadonlySet<string>): PrState {
  const state: PrState = {
    status: 'open',
    open: null,
    updates: [],
    comments: [],
    reviews: [],
    merged: null,
    closed: null,
    timeline: [],
    currentHead: null,
    currentAttachments: []
  };
  for (const entry of ops) {
    const op = entry.op as Record<string, unknown>;
    if (op?.opType !== 'content') {
      continue;
    }
    const payload = op.payload as Record<string, unknown> | undefined;
    const kind = payload?.kind;
    if (!payload || typeof kind !== 'string' || !PR_OP_KINDS.includes(kind as PrOpKind)) {
      continue;
    }
    const actor = actorOf(op);
    const declaredAt = declaredAtOf(op);
    const isTerminal = kind === 'pr.merged' || kind === 'pr.closed';
    const rejectedReason =
      writeSet && isTerminal && !writeSet.has(actor) ? '非写权集合成员的终态操作，未采纳' : undefined;
    state.timeline.push({
      opHash: entry.opHash,
      kind: kind as PrOpKind,
      actor,
      declaredAt,
      payload,
      ...(rejectedReason ? { rejectedReason } : {})
    });
    const base = { actor, declaredAt, opHash: entry.opHash };
    switch (kind as PrOpKind) {
      case 'pr.open': {
        if (state.open) {
          break;
        }
        const attachments = parseAttachments(payload.attachments);
        if (
          typeof payload.title !== 'string' ||
          typeof payload.description !== 'string' ||
          typeof payload.base !== 'string' ||
          !isGitSha(payload.head) ||
          !attachments
        ) {
          break;
        }
        state.open = {
          title: payload.title,
          description: payload.description,
          base: payload.base,
          head: payload.head,
          attachments,
          ...base
        };
        state.currentHead = payload.head;
        state.currentAttachments = attachments;
        break;
      }
      case 'pr.update': {
        if (state.status !== 'open' || !state.open) {
          break;
        }
        const attachments = parseAttachments(payload.attachments);
        if (!isGitSha(payload.head) || !attachments) {
          break;
        }
        const update: PrUpdatePayload & typeof base = {
          head: payload.head,
          attachments,
          ...(typeof payload.note === 'string' ? { note: payload.note } : {}),
          ...base
        };
        state.updates.push(update);
        state.currentHead = payload.head;
        state.currentAttachments = attachments;
        break;
      }
      case 'pr.comment': {
        if (typeof payload.text !== 'string') {
          break;
        }
        const ref = payload.ref as Record<string, unknown> | undefined;
        const comment: PrCommentPayload & typeof base = {
          text: payload.text,
          ...(ref && typeof ref.path === 'string' && Number.isInteger(ref.line)
            ? { ref: { path: ref.path, line: ref.line as number } }
            : {}),
          ...base
        };
        state.comments.push(comment);
        break;
      }
      case 'pr.review': {
        if (typeof payload.text !== 'string' || !['approve', 'request-changes', 'comment'].includes(String(payload.verdict))) {
          break;
        }
        state.reviews.push({ verdict: payload.verdict as PrReviewVerdict, text: payload.text, ...base });
        break;
      }
      case 'pr.merged': {
        if (state.status !== 'open' || !state.open || rejectedReason) {
          break;
        }
        if (!isGitSha(payload.resultCommit) || !Number.isInteger(payload.mirrorVersion)) {
          break;
        }
        state.merged = {
          resultCommit: payload.resultCommit,
          mirrorVersion: payload.mirrorVersion as number,
          ...(typeof payload.note === 'string' ? { note: payload.note } : {}),
          ...base
        };
        state.status = 'merged';
        break;
      }
      case 'pr.closed': {
        if (state.status !== 'open' || !state.open || rejectedReason) {
          break;
        }
        if (typeof payload.reason !== 'string') {
          break;
        }
        state.closed = { reason: payload.reason, ...base };
        state.status = 'closed';
        break;
      }
    }
  }
  return state;
}

/**
 * 合并回执核验（§3.4「已合并」可验证判据）：
 * 回执中的 resultCommit 须出现在新镜像版本（清单分支 head 集合）的历史中；
 * 提供 prHead 时同时核验「PR head ∈ resultCommit 历史」——否则维护者可合入
 * 无关提交后拿任意新 commit 开回执。任何节点可用镜像对象独立重算，不依赖中心裁判。
 */
export function verifyMergeReceipt(
  store: GitObjectStore,
  manifest: MirrorManifest,
  receipt: PrMergedPayload,
  prHead?: string
): { ok: boolean; checkedBranch: string | null; reason: string | null } {
  for (const branch of manifest.branches) {
    if (isAncestor(store, receipt.resultCommit, branch.head)) {
      if (prHead && isGitSha(prHead) && !isAncestor(store, prHead, receipt.resultCommit)) {
        return {
          ok: false,
          checkedBranch: branch.name,
          reason: `回执 commit ${receipt.resultCommit.slice(0, 12)}… 不包含 PR head ${prHead.slice(0, 12)}…（合入的可能不是本 PR 的改动，回执不予采纳）`
        };
      }
      return { ok: true, checkedBranch: branch.name, reason: null };
    }
  }
  return {
    ok: false,
    checkedBranch: null,
    reason: `回执 commit ${receipt.resultCommit.slice(0, 12)}… 不在镜像 v${manifest.version} 任何分支的历史中`
  };
}

/**
 * 写回快进判定（档一-3/O7 禁止非快进）：resultCommit 必须以当前镜像对应
 * 分支 head 为祖先（含相等——无新提交的幂等回执）。
 */
export function checkFastForward(
  store: GitObjectStore,
  manifest: MirrorManifest,
  branchName: string,
  resultCommit: string
): { ok: boolean; reason: string | null } {
  const branch = manifest.branches.find((item) => item.name === branchName) ?? null;
  if (!branch) {
    return { ok: false, reason: `镜像清单中不存在分支 ${branchName}` };
  }
  if (!isAncestor(store, branch.head, resultCommit)) {
    return {
      ok: false,
      reason: `非快进写回被拒绝：结果 commit 不以当前 ${branchName} head（${branch.head.slice(0, 12)}…）为祖先（档一-3 禁止非快进）`
    };
  }
  return { ok: true, reason: null };
}

// ------------------------------------------------------------------
// 输入校验（视图/服务共用，fail-fast 于签名前）
// ------------------------------------------------------------------

export type Verdict = { ok: true } | { ok: false; reason: string };

export function validatePrOpenInput(input: {
  title: string;
  description: string;
  base: string;
  head: string;
  attachments: PrAttachment[];
}): Verdict {
  if (input.title.trim().length < 2 || input.title.trim().length > 120) {
    return { ok: false, reason: 'PR 标题须为 2–120 字符' };
  }
  if (input.description.length > 8000) {
    return { ok: false, reason: 'PR 描述过长（上限 8000 字符）' };
  }
  if (input.base.trim() === '') {
    return { ok: false, reason: '缺少 base 分支' };
  }
  if (!isGitSha(input.head)) {
    return { ok: false, reason: 'head commit sha 形状非法' };
  }
  if (input.attachments.length === 0) {
    return { ok: false, reason: 'PR 至少携带一个 bundle/patch 附件' };
  }
  for (const attachment of input.attachments) {
    if (attachment.size > ATTACHMENT_WARN_BYTES) {
      // 档三-7：提示阈值不阻断，硬上限归内核体积卫生；由视图层提示
      continue;
    }
  }
  return { ok: true };
}

export function validateManifestInput(input: {
  repo: string;
  defaultBranch: string;
  branches: MirrorBranch[];
  version: number;
}): Verdict {
  if (input.repo.trim() === '' || input.repo.length > 120) {
    return { ok: false, reason: '仓库名缺失或过长' };
  }
  if (input.branches.length === 0) {
    return { ok: false, reason: '镜像清单至少包含一个分支' };
  }
  if (!input.branches.some((branch) => branch.name === input.defaultBranch)) {
    return { ok: false, reason: `默认分支 ${input.defaultBranch} 不在分支清单中` };
  }
  if (!Number.isInteger(input.version) || input.version < 1) {
    return { ok: false, reason: '镜像版本号须为正整数' };
  }
  return { ok: true };
}

/** PR 卡片 / 列表的纯文本摘要（「消息自描述」约定：未装插件成员可见） */
export function buildPrSummary(state: { open: { title: string; base: string } | null; status: PrStatus }): string {
  const statusText = state.status === 'open' ? '开放' : state.status === 'merged' ? '已合并' : '已关闭';
  const title = state.open?.title ?? '(PR 元数据未同步)';
  const base = state.open?.base ?? '?';
  return `[PR] ${title}（base: ${base}）— ${statusText}`;
}
