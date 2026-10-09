import { describe, expect, it } from 'vitest';
import {
  CHILD_AFFAIR_TYPES,
  CHILD_TYPE_LABELS,
  DISPOSITION_KIND,
  PROJECT_AFFAIR_TYPE,
  PROJECT_COMMENT_KIND,
  PROJECT_MAX_TAGS,
  PROJECT_RULES_TEMPLATE,
  buildChildRulesDoc,
  buildDispositionCloseCondition,
  buildProjectRulesDoc,
  deriveDisposition,
  deriveDocSummaries,
  docHistory,
  extractMaintainers,
  isChildOfProject,
  parentAffairOf,
  readGenesisMeta,
  resolutionBadgeFromStates,
  sortTimeline,
  toTimelineEntry,
  validateChildInput,
  validateCommentText,
  validateDocInput,
  validateProjectInput,
  type ProjectDocVersion
} from '../model';
import manifestJson from '../manifest.json';

const MAINTAINER_A = 'a'.repeat(64);
const MAINTAINER_B = 'b'.repeat(64);
const OUTSIDER = 'f'.repeat(64);
const PROJECT_ID = 'c'.repeat(64);

const mkOp = (
  opHash: string,
  overrides: { prevOpHash?: string; payload?: Record<string, unknown>; actorIdentity?: string; declaredAt?: number; opType?: string } = {}
): { opHash: string; op: Record<string, unknown> } => ({
  opHash,
  op: {
    opV: 1,
    affairId: 'd'.repeat(32),
    opType: overrides.opType ?? 'content',
    prevOpHash: overrides.prevOpHash ?? 'd'.repeat(32),
    payload: overrides.payload ?? { kind: DISPOSITION_KIND, action: 'adopted' },
    actor: { kind: 'person', identity: overrides.actorIdentity ?? MAINTAINER_A, publicKey: 'cGsx' },
    declaredAt: overrides.declaredAt ?? 1
  }
});

describe('spark-project model · affairTypes 四类独占注册（X8/档一-5）', () => {
  it('manifest affairTypes 恰为 project/bug/proposal/pr，与 model 常量一致', () => {
    expect(manifestJson.affairTypes).toEqual(['project', 'bug', 'proposal', 'pr']);
    expect(PROJECT_AFFAIR_TYPE).toBe('project');
    expect([...CHILD_AFFAIR_TYPES].sort()).toEqual(['bug', 'pr', 'proposal'].sort());
    expect(Object.keys(CHILD_TYPE_LABELS).sort()).toEqual([...CHILD_AFFAIR_TYPES].sort());
  });
});

describe('spark-project model · 输入校验', () => {
  it('project input: 标题/简介必填，长度与标签上限生效', () => {
    expect(validateProjectInput({ title: '  ', summary: 'x', tags: [] }).ok).toBe(false);
    expect(validateProjectInput({ title: 'x', summary: '', tags: [] }).reason).toContain('简介');
    expect(validateProjectInput({ title: 'x'.repeat(121), summary: 's', tags: [] }).ok).toBe(false);
    expect(validateProjectInput({ title: 'x', summary: 's', tags: Array(PROJECT_MAX_TAGS + 1).fill('t') }).ok).toBe(false);
    expect(validateProjectInput({ title: 'x', summary: 's', tags: ['  '] }).ok).toBe(false);
    expect(validateProjectInput({ title: 'x', summary: 's', tags: ['t'.repeat(25)] }).ok).toBe(false);
    expect(validateProjectInput({ title: ' 星火 ', summary: ' s ', tags: ['core'] })).toEqual({ ok: true });
  });

  it('child input: 未知类型 fail-closed（档三-4 未预设类型走通用显示，不走创建）', () => {
    expect(validateChildInput({ type: 'epic', title: 'x', summary: 's' }).ok).toBe(false);
    expect(validateChildInput({ type: 'bug', title: 'x', summary: 's' }).ok).toBe(true);
    expect(validateChildInput({ type: 'proposal', title: 'x', summary: 's' }).ok).toBe(true);
    expect(validateChildInput({ type: 'pr', title: 'x', summary: 's' }).ok).toBe(true);
  });

  it('comment/doc 校验上限', () => {
    expect(validateCommentText('   ').ok).toBe(false);
    expect(validateCommentText('x'.repeat(5001)).ok).toBe(false);
    expect(validateCommentText('正常发言')).toEqual({ ok: true });
    expect(validateDocInput({ title: '', body: 'b' }).ok).toBe(false);
    expect(validateDocInput({ title: 't', body: 'b'.repeat(50001) }).ok).toBe(false);
    expect(validateDocInput({ title: 't', body: '' }).ok).toBe(true); // 正文允许为空（清空也是一种版本）
  });
});

describe('spark-project model · 规则文档（维护者制模板，档一-3 + R1）', () => {
  it('项目规则：maintainers 去重 + PR 治理写明单维护者回执 + 禁非快进', () => {
    const rules = buildProjectRulesDoc([MAINTAINER_A, MAINTAINER_A, MAINTAINER_B]);
    expect(rules.engine).toBe('b1');
    // R1：closeConditions 非空（op-count × project.disposition × 1，内核可判定），
    // pubPeriod 因此是活字段（处置决议公示期）
    expect(rules.closeConditions).toEqual([
      { type: 'op-count', opType: 'content', filter: 'project.disposition', count: 1 }
    ]);
    expect((rules.pubPeriod as Record<string, unknown>).delayMs).toBe(24 * 3600 * 1000);
    expect(rules.maintainers).toEqual([MAINTAINER_A, MAINTAINER_B]);
    const sparkProject = rules.sparkProject as Record<string, unknown>;
    expect(sparkProject.template).toBe(PROJECT_RULES_TEMPLATE);
    expect(sparkProject.prGovernance).toBe('single-maintainer-merge-receipt');
    expect(sparkProject.fastForwardOnly).toBe(true);
    expect(sparkProject.childTypes).toEqual([...CHILD_AFFAIR_TYPES]);
  });

  it('子事务规则：同一关闭条件 + maintainers 快照 + 权威源标注 parent', () => {
    const rules = buildChildRulesDoc([MAINTAINER_A]);
    expect(rules.closeConditions).toEqual([buildDispositionCloseCondition()]);
    expect(rules.maintainers).toEqual([MAINTAINER_A]);
    expect((rules.sparkProject as Record<string, unknown>).maintainerAuthority).toBe('parent');
  });

  it('extractMaintainers：畸形/未声明 → 空集 fail-closed；非法 id 过滤', () => {
    expect(extractMaintainers(null)).toEqual([]);
    expect(extractMaintainers({})).toEqual([]);
    expect(extractMaintainers({ maintainers: 'not-array' })).toEqual([]);
    expect(extractMaintainers({ maintainers: [MAINTAINER_A, 'not-hex', MAINTAINER_A, 42] })).toEqual([MAINTAINER_A]);
  });
});

describe('spark-project model · 创世记录读侧', () => {
  const genesis = {
    affairV: 1,
    type: 'project',
    title: '星火',
    summary: 's',
    tags: ['core', 42],
    refs: [],
    publish: true,
    createdAt: 100
  };

  it('readGenesisMeta：publish 严格布尔（truthy 字符串不算公开，档二-2 fail-closed）', () => {
    const meta = readGenesisMeta(PROJECT_ID, genesis, { following: true, operationCount: 3 });
    expect(meta).toMatchObject({ affairId: PROJECT_ID, type: 'project', isPublic: true, tags: ['core'] });
    expect(readGenesisMeta(PROJECT_ID, { ...genesis, publish: 'yes' }, { following: false, operationCount: 0 })?.isPublic).toBe(false);
    expect(readGenesisMeta(PROJECT_ID, { summary: '无标题' }, { following: false, operationCount: 0 })).toBeNull();
    expect(readGenesisMeta(PROJECT_ID, null, { following: false, operationCount: 0 })).toBeNull();
  });

  it('isChildOfProject / parentAffairOf：refs rel=parent 精确匹配', () => {
    const child = { refs: [{ target: PROJECT_ID, rel: 'parent' }] };
    expect(isChildOfProject(child, PROJECT_ID)).toBe(true);
    expect(parentAffairOf(child)).toBe(PROJECT_ID);
    expect(isChildOfProject({ refs: [{ target: PROJECT_ID, rel: 'related' }] }, PROJECT_ID)).toBe(false);
    expect(isChildOfProject({ refs: [] }, PROJECT_ID)).toBe(false);
    expect(parentAffairOf(null)).toBeNull();
  });
});

describe('spark-project model · 子事务处置推导 · receipt 形态（PR 回执，档一-3）', () => {
  const writeSet = new Set([MAINTAINER_A, MAINTAINER_B]);

  it('无有效处置 → open；写权集合外 actor 的处置不参与推导', () => {
    expect(deriveDisposition([], writeSet, 'receipt')).toEqual({ state: 'open', opHash: null, note: null, actorIdentity: null });
    const forged = [mkOp('zz', { actorIdentity: OUTSIDER, declaredAt: Number.MAX_SAFE_INTEGER })];
    expect(deriveDisposition(forged, writeSet, 'receipt').state).toBe('open');
  });

  it('因果后继优先（不看得更早/更晚的声明时刻）', () => {
    // first: adopted（declaredAt 更晚）；second: closed 挂在 first 之后（declaredAt 更早）
    const first = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted' }, declaredAt: 999 });
    const second = mkOp('bb', { prevOpHash: 'aa', payload: { kind: DISPOSITION_KIND, action: 'closed', note: '重复' }, declaredAt: 1 });
    const view = deriveDisposition([second, first], writeSet, 'receipt');
    expect(view).toMatchObject({ state: 'closed', opHash: 'bb', note: '重复', actorIdentity: MAINTAINER_A });
  });

  it('分叉平局按 opHash 字典序 tie-break（确定性）', () => {
    const a = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted' } });
    const b = mkOp('bb', { payload: { kind: DISPOSITION_KIND, action: 'closed' } });
    const view = deriveDisposition([a, b], writeSet, 'receipt');
    expect(view.opHash).toBe('bb');
    expect(deriveDisposition([b, a], writeSet, 'receipt').opHash).toBe('bb'); // 与输入序无关
  });

  it('畸形载荷（未知 action / 非 content / 缺 actor）留痕不参与', () => {
    const ops = [
      mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'merged' } }),
      mkOp('bb', { opType: 'resolution' }),
      mkOp('cc', { actorIdentity: '' })
    ];
    expect(deriveDisposition(ops, writeSet, 'receipt').state).toBe('open');
  });

  it('空写权集 fail-closed：一切处置都不算（规则不可读时诚实降级为开放）', () => {
    const ops = [mkOp('aa')];
    expect(deriveDisposition(ops, new Set(), 'receipt').state).toBe('open');
  });
});

describe('spark-project model · 子事务处置推导 · resolution 形态（R1：bug/proposal 入公示期）', () => {
  const writeSet = new Set([MAINTAINER_A, MAINTAINER_B]);
  /** 处置动议（content）+ 决议操作（resolution，countedOps 回引动议） */
  const mkResolution = (
    opHash: string,
    result: 'adopted' | 'closed',
    motionHash: string,
    overrides: { actorIdentity?: string; prevOpHash?: string } = {}
  ): { opHash: string; op: Record<string, unknown> } =>
    mkOp(opHash, {
      opType: 'resolution',
      prevOpHash: overrides.prevOpHash ?? motionHash,
      actorIdentity: overrides.actorIdentity,
      payload: {
        result,
        condition: { type: 'op-count', opType: 'content', filter: DISPOSITION_KIND, count: 1 },
        countedOps: [motionHash],
        rulesHash: 'rh',
        pubPeriod: { delayMs: 86400000 }
      }
    });

  it('决议操作生效：状态取自 payload.result；note 从 countedOps 回引的动议恢复', () => {
    const motion = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted', note: '好建议' } });
    const resolution = mkResolution('rr', 'adopted', 'aa');
    const view = deriveDisposition([motion, resolution], writeSet, 'resolution');
    expect(view).toMatchObject({ state: 'adopted', opHash: 'rr', note: '好建议', actorIdentity: MAINTAINER_A });
  });

  it('光有动议无决议 → 保持开放（诚实边界：决议未入日志不提前生效）', () => {
    const motion = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted' } });
    expect(deriveDisposition([motion], writeSet, 'resolution').state).toBe('open');
    // receipt 形态下同一日志回执即生效（形态分流的正反两面）
    expect(deriveDisposition([motion], writeSet, 'receipt').state).toBe('adopted');
  });

  it('写权集合外 actor 的伪造决议不参与推导（readResolution 不含 actor，读侧必须自证）', () => {
    const motion = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted' } });
    const real = mkResolution('rr', 'adopted', 'aa');
    const forged = mkResolution('zz', 'closed', 'aa', { actorIdentity: OUTSIDER, prevOpHash: 'rr' });
    const view = deriveDisposition([motion, real, forged], writeSet, 'resolution');
    expect(view).toMatchObject({ state: 'adopted', opHash: 'rr' });
  });

  it('维护者改判：后序决议（因果后继）覆盖前序', () => {
    const m1 = mkOp('aa', { payload: { kind: DISPOSITION_KIND, action: 'adopted' } });
    const r1 = mkResolution('rr', 'adopted', 'aa');
    const m2 = mkOp('bb', { prevOpHash: 'rr', payload: { kind: DISPOSITION_KIND, action: 'closed', note: '反悔' } });
    const r2 = mkResolution('ss', 'closed', 'bb', { actorIdentity: MAINTAINER_B, prevOpHash: 'bb' });
    const view = deriveDisposition([m1, r1, m2, r2], writeSet, 'resolution');
    expect(view).toMatchObject({ state: 'closed', opHash: 'ss', note: '反悔', actorIdentity: MAINTAINER_B });
  });

  it('result 非处置枚举的决议（如规则修改决议）不参与处置推导', () => {
    const other = mkOp('rr', {
      opType: 'resolution',
      payload: { result: 'passed', condition: {}, countedOps: [], rulesHash: 'rh', pubPeriod: { delayMs: 86400000 } }
    });
    expect(deriveDisposition([other], writeSet, 'resolution').state).toBe('open');
  });
});

describe('spark-project model · 决议徽标（公示期诚实边界）', () => {
  it('effective > pending/unanchored > vetoed > open', () => {
    expect(resolutionBadgeFromStates(['pending', 'effective'])).toBe('effective');
    expect(resolutionBadgeFromStates(['unanchored'])).toBe('pending');
    expect(resolutionBadgeFromStates(['vetoed', 'vetoed'])).toBe('vetoed');
    expect(resolutionBadgeFromStates(['vetoed', 'pending'])).toBe('pending');
    expect(resolutionBadgeFromStates([])).toBe('open');
  });
});

describe('spark-project model · 时间线', () => {
  it('操作 → 条目类型分流（发言/通告/处置/决议/其他）', () => {
    const comment = toTimelineEntry('h1', mkOp('h1', { payload: { kind: PROJECT_COMMENT_KIND, text: '顶一下' } }).op);
    expect(comment).toMatchObject({ kind: 'comment', text: '顶一下' });
    const notice = toTimelineEntry('h2', mkOp('h2', { payload: { kind: 'project.child-notice', childType: 'bug', title: '崩溃', childAffairId: PROJECT_ID } }).op);
    expect(notice.kind).toBe('child-notice');
    expect(notice.text).toContain('缺陷');
    const disposition = toTimelineEntry('h3', mkOp('h3', { payload: { kind: DISPOSITION_KIND, action: 'adopted', note: '好' } }).op);
    expect(disposition).toMatchObject({ kind: 'disposition', action: 'adopted' });
    const resolution = toTimelineEntry('h4', mkOp('h4', { opType: 'resolution', payload: {} }).op);
    expect(resolution.kind).toBe('resolution');
    const dispositionResolution = toTimelineEntry(
      'h4b',
      mkOp('h4b', { opType: 'resolution', payload: { result: 'adopted', condition: {}, countedOps: [], rulesHash: 'rh', pubPeriod: { delayMs: 86400000 } } }).op
    );
    expect(dispositionResolution.text).toContain('已采纳');
    const other = toTimelineEntry('h5', mkOp('h5', { payload: { kind: 'unknown.kind' } }).op);
    expect(other.kind).toBe('other');
  });

  it('展示序 = 声明时刻 + opHash tie-break（仅呈现，不进判定）', () => {
    const entries = [
      toTimelineEntry('zz', mkOp('zz', { declaredAt: 2, payload: { kind: PROJECT_COMMENT_KIND, text: 'b' } }).op),
      toTimelineEntry('aa', mkOp('aa', { declaredAt: 2, payload: { kind: PROJECT_COMMENT_KIND, text: 'a' } }).op),
      toTimelineEntry('mm', mkOp('mm', { declaredAt: 1, payload: { kind: PROJECT_COMMENT_KIND, text: 'c' } }).op)
    ];
    expect(sortTimeline(entries).map((entry) => entry.opHash)).toEqual(['mm', 'aa', 'zz']);
  });
});

describe('spark-project model · 文档版本链（append-only，档三-3 不进决议）', () => {
  const mkVersion = (docId: string, seq: number, title: string, createdAt: number): ProjectDocVersion => ({
    projectAffairId: PROJECT_ID,
    docId,
    seq,
    title,
    body: `body-${seq}`,
    authorIdentity: MAINTAINER_A,
    createdAt
  });

  it('摘要折叠：同 docId 取最大 seq；按更新时间倒序', () => {
    const versions = [
      mkVersion('doc-1', 1, '设计稿', 100),
      mkVersion('doc-1', 3, '设计稿 v3', 300),
      mkVersion('doc-1', 2, '设计稿 v2', 200),
      mkVersion('doc-2', 1, '纪要', 250)
    ];
    const summaries = deriveDocSummaries(versions);
    expect(summaries).toHaveLength(2);
    expect(summaries[0]).toMatchObject({ docId: 'doc-1', latestSeq: 3, title: '设计稿 v3', versionCount: 3 });
    expect(summaries[1].docId).toBe('doc-2');
  });

  it('历史按 seq 升序原样可溯', () => {
    const versions = [mkVersion('doc-1', 2, 'v2', 200), mkVersion('doc-1', 1, 'v1', 100), mkVersion('doc-2', 1, 'x', 50)];
    expect(docHistory(versions, 'doc-1').map((version) => version.seq)).toEqual([1, 2]);
    expect(docHistory(versions, 'missing')).toEqual([]);
  });
});
