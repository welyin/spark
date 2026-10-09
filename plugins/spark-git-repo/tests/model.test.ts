import { describe, expect, it } from 'vitest';
import {
  base64ToBytes,
  buildPrSummary,
  bytesToBase64,
  bytesToUtf8,
  checkFastForward,
  collapseContext,
  derivePrState,
  diffLines,
  diffOversized,
  diffTrees,
  extractWriteSet,
  isAncestor,
  isGitSha,
  latestMirrorManifest,
  listCommits,
  looksBinary,
  mirrorSyncStatus,
  parseCommit,
  parseMirrorManifest,
  parseTree,
  lookupPath,
  utf8ToBytes,
  validateManifestInput,
  validatePrOpenInput,
  verifyMergeReceipt,
  type GitObject,
  type GitObjectStore,
  type MirrorManifest
} from '../model';

// ------------------------------------------------------------------
// 测试夹具：手工构造 git 对象（commit 文本 / tree 二进制线形）
// ------------------------------------------------------------------

const objects = new Map<string, GitObject>();
const store: GitObjectStore = { get: (sha) => objects.get(sha) ?? null };

function sha(seed: number): string {
  return seed.toString(16).padStart(40, '0');
}

function putBlob(shaHex: string, text: string): void {
  objects.set(shaHex, { type: 'blob', bytes: utf8ToBytes(text) });
}

function putCommit(shaHex: string, opts: { tree: string; parents?: string[]; time?: number; message?: string; author?: string }): void {
  const lines = [
    `tree ${opts.tree}`,
    ...(opts.parents ?? []).map((p) => `parent ${p}`),
    `author ${opts.author ?? 'Alice'} <a@b.c> ${opts.time ?? 1700000000} +0800`,
    `committer ${opts.author ?? 'Alice'} <a@b.c> ${opts.time ?? 1700000000} +0800`,
    '',
    opts.message ?? 'commit message'
  ];
  objects.set(shaHex, { type: 'commit', bytes: utf8ToBytes(lines.join('\n')) });
}

function treeBytes(entries: Array<{ mode: string; name: string; sha: string }>): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const entry of entries) {
    parts.push(utf8ToBytes(`${entry.mode} ${entry.name}`));
    parts.push(new Uint8Array([0]));
    const raw = new Uint8Array(20);
    for (let i = 0; i < 20; i++) {
      raw[i] = parseInt(entry.sha.slice(i * 2, i * 2 + 2), 16);
    }
    parts.push(raw);
  }
  const total = parts.reduce((sum, p) => sum + p.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

function putTree(shaHex: string, entries: Array<{ mode: string; name: string; sha: string }>): void {
  objects.set(shaHex, { type: 'tree', bytes: treeBytes(entries) });
}

function reset(): void {
  objects.clear();
}

// ------------------------------------------------------------------

describe('base64 / utf8', () => {
  it('roundtrip（含多字节与二进制）', () => {
    const cases = [utf8ToBytes('hello 世界'), new Uint8Array([0, 1, 2, 255, 254]), new Uint8Array(0)];
    for (const bytes of cases) {
      // jsdom/node 跨域 Uint8Array 构造器不同，按内容比对
      expect([...base64ToBytes(bytesToBase64(bytes))]).toEqual([...bytes]);
    }
  });
});

describe('isGitSha', () => {
  it('接受 40/64 位小写 hex，拒绝其他', () => {
    expect(isGitSha('a'.repeat(40))).toBe(true);
    expect(isGitSha('f'.repeat(64))).toBe(true);
    expect(isGitSha('A'.repeat(40))).toBe(false);
    expect(isGitSha('abc')).toBe(false);
    expect(isGitSha(42)).toBe(false);
  });
});

describe('parseCommit', () => {
  it('解析头与 message（含多 parent）', () => {
    reset();
    putCommit(sha(1), { tree: sha(9), parents: [sha(2), sha(3)], time: 1700000123, message: 'feat: x\n\nbody' });
    const commit = parseCommit(sha(1), objects.get(sha(1))!.bytes);
    expect(commit).not.toBeNull();
    expect(commit!.tree).toBe(sha(9));
    expect(commit!.parents).toEqual([sha(2), sha(3)]);
    expect(commit!.committerTimeMs).toBe(1700000123000);
    expect(commit!.subject).toBe('feat: x');
    expect(commit!.message).toBe('feat: x\n\nbody');
  });

  it('无空行分隔的非法对象返回 null', () => {
    expect(parseCommit(sha(1), utf8ToBytes('tree abc'))).toBeNull();
  });
});

describe('parseTree / lookupPath', () => {
  it('解析二进制 tree（目录模式 40000 无前导零）', () => {
    reset();
    putTree(sha(10), [
      { mode: '100644', name: 'a.txt', sha: sha(11) },
      { mode: '40000', name: 'src', sha: sha(12) }
    ]);
    const entries = parseTree(objects.get(sha(10))!.bytes);
    expect(entries).toHaveLength(2);
    expect(entries[0]).toMatchObject({ name: 'a.txt', type: 'blob', sha: sha(11) });
    expect(entries[1]).toMatchObject({ name: 'src', type: 'tree', sha: sha(12) });
  });

  it('lookupPath 逐段下钻', () => {
    reset();
    putTree(sha(10), [{ mode: '40000', name: 'src', sha: sha(12) }]);
    putTree(sha(12), [{ mode: '100644', name: 'main.ts', sha: sha(13) }]);
    const hit = lookupPath(store, sha(10), 'src/main.ts');
    expect(hit?.sha).toBe(sha(13));
    expect(lookupPath(store, sha(10), 'src/missing.ts')).toBeNull();
    expect(lookupPath(store, sha(10), 'src/main.ts/deeper')).toBeNull();
  });
});

describe('listCommits / isAncestor', () => {
  function graph(): void {
    reset();
    // c1 ← c2 ← c3(head)；c2 另有分支 c4
    putTree(sha(90), []);
    putCommit(sha(1), { tree: sha(90), time: 1700000001, message: 'c1' });
    putCommit(sha(2), { tree: sha(90), parents: [sha(1)], time: 1700000002, message: 'c2' });
    putCommit(sha(3), { tree: sha(90), parents: [sha(2)], time: 1700000003, message: 'c3' });
    putCommit(sha(4), { tree: sha(90), parents: [sha(2)], time: 1700000004, message: 'c4' });
  }

  it('历史按时间降序返回', () => {
    graph();
    const history = listCommits(store, sha(3));
    expect(history.map((c) => c.sha)).toEqual([sha(3), sha(2), sha(1)]);
  });

  it('缺对象的分支如实截断（不伪造连续历史）', () => {
    graph();
    objects.delete(sha(1));
    const history = listCommits(store, sha(3));
    expect(history.map((c) => c.sha)).toEqual([sha(3), sha(2)]);
  });

  it('祖先判定（含自身；旁支不是祖先）', () => {
    graph();
    expect(isAncestor(store, sha(1), sha(3))).toBe(true);
    expect(isAncestor(store, sha(3), sha(3))).toBe(true);
    expect(isAncestor(store, sha(4), sha(3))).toBe(false);
    expect(isAncestor(store, sha(3), sha(1))).toBe(false);
  });
});

describe('diffLines / collapseContext', () => {
  it('新增/删除/上下文行', () => {
    const ops = diffLines('a\nb\nc', 'a\nx\nc');
    expect(ops).toEqual([
      { type: 'context', text: 'a' },
      { type: 'del', text: 'b' },
      { type: 'add', text: 'x' },
      { type: 'context', text: 'c' }
    ]);
  });

  it('空 ↔ 非空', () => {
    expect(diffLines('', 'a\nb')).toEqual([
      { type: 'add', text: 'a' },
      { type: 'add', text: 'b' }
    ]);
    expect(diffLines('a', '')).toEqual([{ type: 'del', text: 'a' }]);
    expect(diffLines('', '')).toEqual([]);
  });

  it('collapseContext 折叠长上下文', () => {
    const ops = diffLines(['1', '2', '3', '4', '5', '6', '7', '8', '9', 'x'].join('\n'), ['1', '2', '3', '4', '5', '6', '7', '8', '9', 'y'].join('\n'));
    const rows = collapseContext(ops, 2);
    expect(rows.some((row) => row.type === 'gap')).toBe(true);
    expect(rows.filter((row) => row.type === 'add' || row.type === 'del')).toHaveLength(2);
  });
});

describe('diffTrees', () => {
  it('新增/删除/修改/嵌套', () => {
    reset();
    putBlob(sha(21), 'v1');
    putBlob(sha(22), 'v2');
    putBlob(sha(23), 'new');
    putTree(sha(31), [{ mode: '100644', name: 'keep.txt', sha: sha(21) }]);
    putTree(sha(32), [{ mode: '100644', name: 'keep.txt', sha: sha(22) }, { mode: '100644', name: 'add.txt', sha: sha(23) }]);
    putTree(sha(41), [
      { mode: '100644', name: 'del.txt', sha: sha(21) },
      { mode: '40000', name: 'src', sha: sha(31) }
    ]);
    putTree(sha(42), [{ mode: '40000', name: 'src', sha: sha(32) }]);
    const changes = diffTrees(store, sha(41), sha(42));
    expect(changes).toEqual([
      { path: 'del.txt', change: 'removed', oldSha: sha(21) },
      { path: 'src/add.txt', change: 'added', newSha: sha(23) },
      { path: 'src/keep.txt', change: 'modified', oldSha: sha(21), newSha: sha(22) }
    ]);
  });
});

describe('looksBinary', () => {
  it('NUL 判定', () => {
    expect(looksBinary(utf8ToBytes('plain text'))).toBe(false);
    expect(looksBinary(new Uint8Array([0x89, 0x50, 0, 0x0d]))).toBe(true);
  });
});

// ------------------------------------------------------------------
// 镜像清单
// ------------------------------------------------------------------

const CID = 'c'.repeat(64);

function manifestPayload(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    kind: 'git.mirror.manifest',
    repo: 'spark',
    defaultBranch: 'main',
    branches: [{ name: 'main', head: 'a'.repeat(40) }],
    version: 1,
    objects: [{ sha: 'a'.repeat(40), type: 'commit', cid: CID }],
    ...overrides
  };
}

function manifestOp(version: number, opHash: string, overrides: Record<string, unknown> = {}) {
  return { opHash, op: { opType: 'content', payload: manifestPayload({ version, ...overrides }) } };
}

describe('parseMirrorManifest', () => {
  it('合法载荷解析', () => {
    const manifest = parseMirrorManifest(manifestPayload({ importHead: 'b'.repeat(40) }));
    expect(manifest).toMatchObject({ repo: 'spark', version: 1, defaultBranch: 'main', importHead: 'b'.repeat(40) });
    expect(manifest!.objects[0].cid).toBe(CID);
  });

  it.each([
    ['缺 branches', { branches: [] }],
    ['version 非正整数', { version: 0 }],
    ['head 形状非法', { branches: [{ name: 'main', head: 'xyz' }] }],
    ['cid 形状非法', { objects: [{ sha: 'a'.repeat(40), type: 'commit', cid: 'zz' }] }],
    ['对象 type 非法', { objects: [{ sha: 'a'.repeat(40), type: 'module', cid: CID }] }],
    ['kind 不符', { kind: 'pr.open' }]
  ])('形状非法返回 null：%s', (_label, patch) => {
    expect(parseMirrorManifest(manifestPayload(patch))).toBeNull();
  });
});

describe('latestMirrorManifest', () => {
  it('取最大 version；同号取 opHash 大者并计冲突', () => {
    const ops = [
      manifestOp(2, 'aa'.repeat(32)),
      manifestOp(1, 'bb'.repeat(32)),
      manifestOp(2, 'cc'.repeat(32))
    ];
    const latest = latestMirrorManifest(ops);
    expect(latest).not.toBeNull();
    expect(latest!.manifest.version).toBe(2);
    expect(latest!.opHash).toBe('cc'.repeat(32));
    expect(latest!.conflicts).toBe(1);
  });

  it('无关操作跳过；空日志返回 null', () => {
    expect(latestMirrorManifest([{ opHash: 'x', op: { opType: 'content', payload: { kind: 'pr.comment' } } }])).toBeNull();
    expect(latestMirrorManifest([])).toBeNull();
  });
});

describe('mirrorSyncStatus', () => {
  it('本地持有度如实统计', () => {
    const manifest = parseMirrorManifest(manifestPayload())!;
    const status = mirrorSyncStatus(manifest, new Set());
    expect(status).toEqual({ total: 1, local: 0, missing: [CID] });
    expect(mirrorSyncStatus(manifest, new Set([CID])).local).toBe(1);
  });
});

// ------------------------------------------------------------------
// PR 状态推导
// ------------------------------------------------------------------

function prOp(kind: string, opHash: string, payload: Record<string, unknown>, actor = 'id-actor') {
  return {
    opHash,
    op: {
      opType: 'content',
      payload: { kind, ...payload },
      actor: { kind: 'person', identity: actor, publicKey: 'pk' },
      declaredAt: 1700000000000
    }
  };
}

const PR_OPEN = {
  title: '修复空指针',
  description: '详见附件',
  base: 'main',
  head: 'a'.repeat(40),
  attachments: [{ kind: 'bundle', cid: CID, size: 1024, name: 'fix.bundle' }]
};

describe('derivePrState', () => {
  it('完整生命周期：open → update → comment → review → merged', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.update', '02'.repeat(32), { head: 'b'.repeat(40), attachments: [{ kind: 'bundle', cid: 'd'.repeat(64), size: 2048 }], note: 'v2' }),
      prOp('pr.comment', '03'.repeat(32), { text: '看一下', ref: { path: 'src/a.ts', line: 3 } }),
      prOp('pr.review', '04'.repeat(32), { verdict: 'approve', text: 'LGTM' }),
      prOp('pr.merged', '05'.repeat(32), { resultCommit: 'c'.repeat(40), mirrorVersion: 3 })
    ];
    const state = derivePrState(ops);
    expect(state.status).toBe('merged');
    expect(state.open?.title).toBe('修复空指针');
    expect(state.updates).toHaveLength(1);
    expect(state.currentHead).toBe('b'.repeat(40));
    expect(state.currentAttachments[0].cid).toBe('d'.repeat(64));
    expect(state.comments[0].ref).toEqual({ path: 'src/a.ts', line: 3 });
    expect(state.reviews[0].verdict).toBe('approve');
    expect(state.merged).toMatchObject({ resultCommit: 'c'.repeat(40), mirrorVersion: 3 });
    expect(state.timeline).toHaveLength(5);
  });

  it('closed 终态；首个关闭操作生效，后续操作只留 timeline', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.closed', '02'.repeat(32), { reason: '重复 PR' }),
      prOp('pr.merged', '03'.repeat(32), { resultCommit: 'c'.repeat(40), mirrorVersion: 4 })
    ];
    const state = derivePrState(ops);
    expect(state.status).toBe('closed');
    expect(state.closed?.reason).toBe('重复 PR');
    expect(state.merged).toBeNull();
    expect(state.timeline).toHaveLength(3);
  });

  it('重复 pr.open 忽略；形状非法操作跳过状态但留 timeline', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.open', '02'.repeat(32), { ...PR_OPEN, title: '篡改标题' }),
      prOp('pr.update', '03'.repeat(32), { head: 'not-a-sha' }),
      { opHash: '04'.repeat(32), op: { opType: 'content', payload: { kind: 'unrelated' } } }
    ];
    const state = derivePrState(ops);
    expect(state.open?.title).toBe('修复空指针');
    expect(state.updates).toHaveLength(0);
    expect(state.timeline).toHaveLength(3);
  });
});

describe('derivePrState 写权集合（档一-3 硬伤修复）', () => {
  const MAINTAINER = 'ab'.repeat(32);
  const OUTSIDER = 'cd'.repeat(32);

  it('伪造 pr.merged / pr.closed（集合外 actor）→ 不采纳终态，留 timeline 并标注', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.merged', '02'.repeat(32), { resultCommit: 'c'.repeat(40), mirrorVersion: 2 }, OUTSIDER),
      prOp('pr.closed', '03'.repeat(32), { reason: '我就是要关' }, OUTSIDER)
    ];
    const state = derivePrState(ops, new Set([MAINTAINER]));
    expect(state.status).toBe('open');
    expect(state.merged).toBeNull();
    expect(state.closed).toBeNull();
    expect(state.timeline).toHaveLength(3);
    expect(state.timeline[1].rejectedReason).toContain('非写权集合成员');
    expect(state.timeline[2].rejectedReason).toContain('非写权集合成员');
  });

  it('写权集合内 actor 的 merged/closed 正常采纳；集合外伪造在前不抢占终态', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.closed', '02'.repeat(32), { reason: '伪造关闭' }, OUTSIDER),
      prOp('pr.merged', '03'.repeat(32), { resultCommit: 'c'.repeat(40), mirrorVersion: 2 }, MAINTAINER)
    ];
    const state = derivePrState(ops, new Set([MAINTAINER]));
    expect(state.status).toBe('merged');
    expect(state.merged?.actor).toBe(MAINTAINER);
    expect(state.timeline[1].rejectedReason).toBeDefined();
    expect(state.timeline[2].rejectedReason).toBeUndefined();
  });

  it('空写权集合 → 无人的终态操作被采纳（fail-closed）；writeSet 缺省保持旧口径', () => {
    const ops = [
      prOp('pr.open', '01'.repeat(32), PR_OPEN),
      prOp('pr.merged', '02'.repeat(32), { resultCommit: 'c'.repeat(40), mirrorVersion: 2 }, MAINTAINER)
    ];
    expect(derivePrState(ops, new Set()).status).toBe('open');
    expect(derivePrState(ops).status).toBe('merged');
  });
});

describe('extractWriteSet（rules.maintainers 约定）', () => {
  it('提取 64-hex 身份 id；缺字段 / 形状非法 / 非 hex 一律过滤为空集', () => {
    expect(extractWriteSet({ maintainers: ['ab'.repeat(32), 'cd'.repeat(32)] })).toEqual(['ab'.repeat(32), 'cd'.repeat(32)]);
    expect(extractWriteSet({})).toEqual([]);
    expect(extractWriteSet(null)).toEqual([]);
    expect(extractWriteSet({ maintainers: 'not-an-array' })).toEqual([]);
    expect(extractWriteSet({ maintainers: ['id-not-hex', 42, 'ab'.repeat(32)] })).toEqual(['ab'.repeat(32)]);
  });
});

describe('diffOversized（大 diff 护栏）', () => {
  it('小文本放过；超过 256KB 合计判 oversized', () => {
    expect(diffOversized('a\nb', 'a\nc')).toBe(false);
    expect(diffOversized('x'.repeat(200 * 1024), 'y'.repeat(100 * 1024))).toBe(true);
    expect(diffOversized('', 'y'.repeat(300 * 1024))).toBe(true);
  });
});

// ------------------------------------------------------------------
// FF 判定与合并回执核验
// ------------------------------------------------------------------

describe('checkFastForward / verifyMergeReceipt', () => {
  function mergedGraph(): MirrorManifest {
    reset();
    putTree(sha(90), []);
    // old head = sha(2)，合并结果 sha(5)（parents: sha(2), sha(3)）
    putCommit(sha(1), { tree: sha(90), time: 1 });
    putCommit(sha(2), { tree: sha(90), parents: [sha(1)], time: 2 });
    putCommit(sha(3), { tree: sha(90), parents: [sha(1)], time: 3 });
    putCommit(sha(5), { tree: sha(90), parents: [sha(2), sha(3)], time: 5 });
    putCommit(sha(6), { tree: sha(90), time: 6 }); // 无父（重写历史形态）
    return {
      repo: 'spark',
      defaultBranch: 'main',
      branches: [{ name: 'main', head: sha(5) }],
      version: 2,
      objects: []
    };
  }

  it('合并提交以旧 head 为父 → FF 通过', () => {
    const manifest = mergedGraph();
    // 旧清单 head = sha(2)
    const oldManifest = { ...manifest, branches: [{ name: 'main', head: sha(2) }] };
    expect(checkFastForward(store, oldManifest, 'main', sha(5)).ok).toBe(true);
  });

  it('重写历史（新 head 不以旧 head 为祖先）→ 拒绝非快进', () => {
    const manifest = mergedGraph();
    const oldManifest = { ...manifest, branches: [{ name: 'main', head: sha(2) }] };
    const verdict = checkFastForward(store, oldManifest, 'main', sha(6));
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain('非快进');
  });

  it('分支不存在 → 拒绝', () => {
    const manifest = mergedGraph();
    expect(checkFastForward(store, manifest, 'dev', sha(5)).ok).toBe(false);
  });

  it('回执 commit ∈ 新镜像历史 → 核验通过；否则失败并给出原因', () => {
    const manifest = mergedGraph();
    expect(verifyMergeReceipt(store, manifest, { resultCommit: sha(3), mirrorVersion: 2 }).ok).toBe(true);
    const bad = verifyMergeReceipt(store, manifest, { resultCommit: sha(6), mirrorVersion: 2 });
    expect(bad.ok).toBe(false);
    expect(bad.reason).toContain('不在镜像');
  });

  it('加强判据（建议 8）：PR head 不在 resultCommit 历史中 → 不采纳', () => {
    const manifest = mergedGraph();
    // resultCommit=sha(5) 在 main 历史中，且包含 PR head sha(3) → 通过
    expect(verifyMergeReceipt(store, manifest, { resultCommit: sha(5), mirrorVersion: 2 }, sha(3)).ok).toBe(true);
    // sha(3) 在 main 历史中但不包含 PR head sha(2)（拿无关提交开回执）→ 拒绝
    const verdict = verifyMergeReceipt(store, manifest, { resultCommit: sha(3), mirrorVersion: 2 }, sha(2));
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain('不包含 PR head');
  });
});

// ------------------------------------------------------------------
// 输入校验与摘要
// ------------------------------------------------------------------

describe('validatePrOpenInput', () => {
  const valid = { title: '标题', description: '', base: 'main', head: 'a'.repeat(40), attachments: [{ kind: 'bundle' as const, cid: CID, size: 1 }] };
  it('合法输入通过', () => {
    expect(validatePrOpenInput(valid).ok).toBe(true);
  });
  it.each([
    ['标题过短', { title: 'x' }],
    ['head 非法', { head: 'zz' }],
    ['无附件', { attachments: [] }],
    ['缺 base', { base: ' ' }]
  ])('拒绝：%s', (_label, patch) => {
    expect(validatePrOpenInput({ ...valid, ...patch }).ok).toBe(false);
  });
});

describe('validateManifestInput', () => {
  it('默认分支须在分支清单中', () => {
    expect(validateManifestInput({ repo: 'r', defaultBranch: 'main', branches: [{ name: 'dev', head: 'a'.repeat(40) }], version: 1 }).ok).toBe(false);
    expect(validateManifestInput({ repo: 'r', defaultBranch: 'main', branches: [{ name: 'main', head: 'a'.repeat(40) }], version: 1 }).ok).toBe(true);
  });
});

describe('buildPrSummary', () => {
  it('未装插件成员可见的纯文本摘要', () => {
    expect(buildPrSummary({ open: { title: 'T', base: 'main' }, status: 'merged' })).toBe('[PR] T（base: main）— 已合并');
    expect(buildPrSummary({ open: null, status: 'open' })).toContain('元数据未同步');
  });
});

describe('bytesToUtf8 覆盖（导出完整性）', () => {
  it('utf8 解码', () => {
    expect(bytesToUtf8(utf8ToBytes('星火'))).toBe('星火');
  });
});
