import { describe, expect, it } from 'vitest';
import {
  buildMirrorManifestPayload,
  buildOpDraft,
  buildParentRef,
  buildPrClosedPayload,
  buildPrCommentPayload,
  buildPrMergedPayload,
  buildPrOpenPayload,
  buildPrReviewPayload,
  buildPrUpdatePayload,
  deriveIdentity,
  signPayload
} from '../wire';

const ACTOR = { kind: 'person', identity: 'id-1', publicKey: 'pk-1' };
const AFFAIR_ID = 'ab'.repeat(32);
const CID = 'c'.repeat(64);
const HEAD = 'a'.repeat(40);

describe('buildParentRef', () => {
  it('PR 子事务 → 项目议题为 parent 引用（affair.md §10）', () => {
    expect(buildParentRef(AFFAIR_ID)).toEqual({ target: AFFAIR_ID, rel: 'parent' });
  });
});

describe('PR 操作载荷', () => {
  it('pr.open 线形（git-repo.md §3.2）', () => {
    const payload = buildPrOpenPayload({
      title: '  标题  ',
      description: 'desc',
      base: 'main',
      head: HEAD,
      attachments: [{ kind: 'bundle', cid: CID, size: 10, name: 'x.bundle' }]
    });
    expect(payload).toEqual({
      kind: 'pr.open',
      title: '标题',
      description: 'desc',
      base: 'main',
      head: HEAD,
      attachments: [{ kind: 'bundle', cid: CID, size: 10, name: 'x.bundle' }]
    });
  });

  it('pr.update / pr.comment / pr.review / pr.merged / pr.closed', () => {
    expect(buildPrUpdatePayload({ head: HEAD, attachments: [{ kind: 'bundle', cid: CID, size: 1 }], note: 'n' })).toEqual({
      kind: 'pr.update', head: HEAD, attachments: [{ kind: 'bundle', cid: CID, size: 1 }], note: 'n'
    });
    // 可选字段缺省不携带
    expect(buildPrUpdatePayload({ head: HEAD, attachments: [{ kind: 'bundle', cid: CID, size: 1 }] })).not.toHaveProperty('note');
    expect(buildPrCommentPayload({ text: 't', ref: { path: 'a.ts', line: 2 } })).toEqual({
      kind: 'pr.comment', text: 't', ref: { path: 'a.ts', line: 2 }
    });
    expect(buildPrCommentPayload({ text: 't' })).not.toHaveProperty('ref');
    expect(buildPrReviewPayload({ verdict: 'approve', text: 'LGTM' })).toEqual({ kind: 'pr.review', verdict: 'approve', text: 'LGTM' });
    expect(buildPrMergedPayload({ resultCommit: HEAD, mirrorVersion: 2 })).toEqual({
      kind: 'pr.merged', resultCommit: HEAD, mirrorVersion: 2
    });
    expect(buildPrClosedPayload({ reason: '重复' })).toEqual({ kind: 'pr.closed', reason: '重复' });
  });
});

describe('buildMirrorManifestPayload', () => {
  it('清单载荷（档二-5：议题内签名事务操作）', () => {
    const payload = buildMirrorManifestPayload({
      repo: 'spark',
      defaultBranch: 'main',
      branches: [{ name: 'main', head: HEAD }],
      version: 1,
      objects: [{ sha: HEAD, type: 'commit', cid: CID }],
      importHead: HEAD
    });
    expect(payload).toEqual({
      kind: 'git.mirror.manifest',
      repo: 'spark',
      defaultBranch: 'main',
      branches: [{ name: 'main', head: HEAD }],
      version: 1,
      objects: [{ sha: HEAD, type: 'commit', cid: CID }],
      importHead: HEAD
    });
  });
});

describe('buildOpDraft', () => {
  it('首条操作 prevOpHash = affairId；有头取字典序最小头', () => {
    const first = buildOpDraft(AFFAIR_ID, ACTOR, { kind: 'pr.open' }, [], 123);
    expect(first).toMatchObject({
      opV: 1,
      affairId: AFFAIR_ID,
      opType: 'content',
      prevOpHash: AFFAIR_ID,
      declaredAt: 123,
      actor: { kind: 'person', identity: 'id-1', publicKey: 'pk-1' }
    });
    expect(first).not.toHaveProperty('sig');
    const next = buildOpDraft(AFFAIR_ID, ACTOR, { kind: 'pr.comment' }, ['zz'.repeat(32), 'aa'.repeat(32)], 124);
    expect(next.prevOpHash).toBe('aa'.repeat(32));
  });
});

describe('signPayload / deriveIdentity（SDK affair-wire re-export 完整性）', () => {
  it('签名载荷 = canonical(记录剔除 sig)，身份推导确定性', () => {
    const record = { a: 1, sig: 'should-be-excluded', nested: { b: [2, 3] } };
    const payload = signPayload(record);
    expect(payload).not.toContain('should-be-excluded');
    expect(typeof deriveIdentity('cHVibGljLWtleQ==')).toBe('string');
    expect(deriveIdentity('cHVibGljLWtleQ==')).toHaveLength(64);
    expect(deriveIdentity('cHVibGljLWtleQ==')).toBe(deriveIdentity('cHVibGljLWtleQ=='));
  });
});
