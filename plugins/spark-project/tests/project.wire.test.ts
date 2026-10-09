import { describe, expect, it } from 'vitest';
import {
  buildChildGenesisInput,
  buildChildNoticePayload,
  buildCommentPayload,
  buildDispositionPayload,
  buildOpDraft,
  buildProjectGenesisInput,
  buildResolutionDraft,
  deriveIdentity,
  signPayload
} from '../wire';
import { CHILD_AFFAIR_TYPES, DISPOSITION_KIND, PROJECT_COMMENT_KIND, buildDispositionCloseCondition } from '../model';

const ACTOR = { kind: 'person' as const, identity: 'a'.repeat(64), publicKey: 'cGsx' };
const PROJECT_ID = 'c'.repeat(64);
const BUNDLE_CID = 'e'.repeat(64);

describe('spark-project wire · 项目议题创世输入（§3.1 + 档二-2 publish 声明位）', () => {
  it('type=project；文本规整；标签过滤空白；maintainers 初始 = 发起人插件域身份 id（补录口径）', () => {
    const input = buildProjectGenesisInput(
      { title: ' 星火 ', summary: ' 自举 ', tags: [' core ', ' ', 'p2p'] },
      ACTOR.identity
    );
    expect(input.type).toBe('project');
    expect(input.title).toBe('星火');
    expect(input.summary).toBe('自举');
    expect(input.tags).toEqual(['core', 'p2p']);
    expect((input.rules as Record<string, unknown>).maintainers).toEqual([ACTOR.identity]);
    expect(input.publish).toBeUndefined(); // 缺省不携带 publish（fail-closed，显式声明才公开）
  });

  it('publish 严格布尔：仅 === true 时进创世（档二-2 补录 fail-closed）', () => {
    expect(buildProjectGenesisInput({ title: 't', summary: 's', tags: [], publish: true }, ACTOR.identity).publish).toBe(true);
    expect(buildProjectGenesisInput({ title: 't', summary: 's', tags: [], publish: false }, ACTOR.identity).publish).toBeUndefined();
    // 调用方把 truthy 非布尔值混进来也不放行（类型层之外的红线复核）
    expect(
      buildProjectGenesisInput({ title: 't', summary: 's', tags: [], publish: 'yes' as unknown as boolean }, ACTOR.identity).publish
    ).toBeUndefined();
  });
});

describe('spark-project wire · 子事务创世输入（refs rel=parent 父子引用，§3.4）', () => {
  it('refs 指向项目议题；rules 快照 maintainers；反馈回流同一线形（档一-1）', () => {
    for (const type of CHILD_AFFAIR_TYPES) {
      const input = buildChildGenesisInput(PROJECT_ID, { type, title: ' t ', summary: ' s ' }, [ACTOR.identity]);
      expect(input.type).toBe(type);
      expect(input.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
      expect((input.rules as Record<string, unknown>).maintainers).toEqual([ACTOR.identity]);
      expect((input.extra as Record<string, unknown>).sparkProject).toEqual({ childAffair: true });
      expect((input.extra as Record<string, unknown>).pr).toBeUndefined(); // 非 PR 或无 cid 不携带
    }
  });

  it('PR 载荷：bundle cid 入 extra.pr（随 affairId 被承诺）', () => {
    const input = buildChildGenesisInput(PROJECT_ID, { type: 'pr', title: 't', summary: 's', bundleCid: ` ${BUNDLE_CID} ` }, []);
    expect((input.extra as Record<string, unknown>).pr).toEqual({ bundleCid: BUNDLE_CID });
    const noCid = buildChildGenesisInput(PROJECT_ID, { type: 'pr', title: 't', summary: 's', bundleCid: '  ' }, []);
    expect((noCid.extra as Record<string, unknown>).pr).toBeUndefined();
  });
});

describe('spark-project wire · 内容操作载荷（opType=content，payload.kind 承载语义）', () => {
  it('comment/child-notice/disposition 载荷构造（空白 note 不入载荷）', () => {
    expect(buildCommentPayload(' 顶一下 ')).toEqual({ kind: PROJECT_COMMENT_KIND, text: '顶一下' });
    expect(buildChildNoticePayload({ childAffairId: PROJECT_ID, childType: 'bug', title: '崩溃' })).toEqual({
      kind: 'project.child-notice',
      childAffairId: PROJECT_ID,
      childType: 'bug',
      title: '崩溃'
    });
    expect(buildDispositionPayload('adopted', ' 好 ')).toEqual({ kind: DISPOSITION_KIND, action: 'adopted', note: '好' });
    expect(buildDispositionPayload('closed', '   ')).toEqual({ kind: DISPOSITION_KIND, action: 'closed' });
    expect(buildDispositionPayload('closed')).toEqual({ kind: DISPOSITION_KIND, action: 'closed' });
  });

  it('op 草稿：prevOpHash 取字典序最小 DAG 头；首条 = affairId（affair.md §3.2）', () => {
    const draft = buildOpDraft(PROJECT_ID, ACTOR, buildCommentPayload('x'), ['zz', 'aa'], 1234);
    expect(draft).toMatchObject({
      opV: 1,
      affairId: PROJECT_ID,
      opType: 'content',
      prevOpHash: 'aa',
      declaredAt: 1234,
      actor: ACTOR
    });
    expect((draft.payload as Record<string, unknown>).kind).toBe(PROJECT_COMMENT_KIND);
    const first = buildOpDraft(PROJECT_ID, ACTOR, buildCommentPayload('x'), [], 1);
    expect(first.prevOpHash).toBe(PROJECT_ID);
    expect('sig' in draft).toBe(false); // 草稿不含签名（签名由 service 层追加）
  });

  it('sign payload 剔除 sig 字段（canonical 承诺最小化）', () => {
    expect(signPayload({ a: 1, sig: 'drop-me' })).toBe(signPayload({ a: 1 }));
  });

  it('deriveIdentity = sha256hex(base64decode(publicKey))（§2.2 自包含绑定）', () => {
    expect(deriveIdentity('cGsx')).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe('spark-project wire · 处置决议操作草稿（R1：opType=resolution 入内核公示期）', () => {
  const plan = {
    result: 'adopted' as const,
    condition: buildDispositionCloseCondition(),
    countedOps: ['b'.repeat(64), 'a'.repeat(64)],
    rulesHash: '9'.repeat(64),
    pubPeriodMs: 86400000
  };

  it('线形对齐 affair.md §6.1（spark-affairs buildResolutionDraft 先例）：countedOps §8 升序、pubPeriod 对象形态', () => {
    const draft = buildResolutionDraft(PROJECT_ID, ACTOR, plan, 'a'.repeat(64), 1234);
    expect(draft).toMatchObject({
      opV: 1,
      affairId: PROJECT_ID,
      opType: 'resolution',
      prevOpHash: 'a'.repeat(64), // = 处置动议 opHash（countedOps 须在祖先闭包内）
      declaredAt: 1234,
      actor: ACTOR
    });
    const payload = draft.payload as Record<string, unknown>;
    expect(payload.result).toBe('adopted');
    expect(payload.condition).toEqual({ type: 'op-count', opType: 'content', filter: 'project.disposition', count: 1 });
    expect(payload.countedOps).toEqual(['a'.repeat(64), 'b'.repeat(64)]); // 构造侧强制升序
    expect(payload.rulesHash).toBe('9'.repeat(64));
    expect(payload.pubPeriod).toEqual({ delayMs: 86400000 });
    expect('tally' in payload).toBe(false); // 单维护者动议无计票明细，诚实为空
    expect('sig' in draft).toBe(false);
  });
});
