import { describe, expect, it } from 'vitest';
import {
  AFFAIR_SUMMARY_MAX_LENGTH,
  AFFAIR_TITLE_MAX_LENGTH,
  buildAffairSummary,
  canComment,
  canSubmitContribution,
  canVote,
  evaluateTally,
  planResolution,
  readGenesisMeta,
  rulesFromGenesis,
  sortOperations,
  toAffairOperation,
  toExecStateViews,
  toLadderState,
  toOrgEffectsView,
  toResolutionView,
  toRulesChainView,
  validateAffairDraft,
  validateCommentText,
  validateContributionText,
  type AffairCreateInput,
  type AffairOperation
} from '../model';

function validDraft(overrides: Partial<AffairCreateInput> = {}): AffairCreateInput {
  return {
    title: '小区绿植补种预算',
    summary: '讨论南广场绿植补种的预算方案。',
    tags: ['预算'],
    refs: [],
    rules: {
      reviewPeriodHours: 24,
      passThreshold: 0.67,
      minQuorum: 3,
      initialVoters: [],
      entryRequirement: { kind: 'none' }
    },
    ...overrides
  };
}

describe('spark-affairs model: draft validation', () => {
  it('validates affair draft (title/summary required, thresholds bounded)', () => {
    expect(validateAffairDraft(validDraft()).ok).toBe(true);

    expect(validateAffairDraft(validDraft({ title: '  ' }))).toEqual({
      ok: false,
      reason: '标题不能为空'
    });
    expect(validateAffairDraft(validDraft({ title: 'x'.repeat(AFFAIR_TITLE_MAX_LENGTH + 1) })).ok).toBe(false);
    expect(validateAffairDraft(validDraft({ summary: '' })).ok).toBe(false);
    expect(validateAffairDraft(validDraft({ summary: 'x'.repeat(AFFAIR_SUMMARY_MAX_LENGTH + 1) })).ok).toBe(false);

    const badThreshold = validDraft();
    badThreshold.rules.passThreshold = 1.5;
    expect(validateAffairDraft(badThreshold).ok).toBe(false);

    const badQuorum = validDraft();
    badQuorum.rules.minQuorum = 0;
    expect(validateAffairDraft(badQuorum).ok).toBe(false);
  });

  it('rejects empty tags and over-count tags', () => {
    expect(validateAffairDraft(validDraft({ tags: [' '] })).ok).toBe(false);
    expect(validateAffairDraft(validDraft({ tags: Array.from({ length: 9 }, (_, i) => `t${i}`) })).ok).toBe(false);
  });

  it('enforces the 24h pub-period protocol floor at the form level', () => {
    const draft = validDraft();
    draft.rules.reviewPeriodHours = 12;
    expect(validateAffairDraft(draft)).toEqual({
      ok: false,
      reason: '公示期不能低于 24 小时（协议下限，吸收时钟偏差与副本滞后）'
    });
    draft.rules.reviewPeriodHours = 24;
    expect(validateAffairDraft(draft).ok).toBe(true);
  });

  it('requires initial voters to be 64-hex identity ids', () => {
    const draft = validDraft();
    draft.rules.initialVoters = ['not-an-identity'];
    expect(validateAffairDraft(draft).ok).toBe(false);
    draft.rules.initialVoters = ['ab'.repeat(32)];
    expect(validateAffairDraft(draft).ok).toBe(true);
  });

  it('validates refs shape (§10 rel enum + 64-hex target; self-reference ban is kernel-side)', () => {
    expect(validateAffairDraft(validDraft({ refs: [{ target: 'cd'.repeat(32), rel: 'inherit' }] })).ok).toBe(true);
    expect(validateAffairDraft(validDraft({ refs: [{ target: 'xyz', rel: 'related' }] })).ok).toBe(false);
    expect(validateAffairDraft(validDraft({ refs: [{ target: 'cd'.repeat(32), rel: 'child' as never }] })).ok).toBe(false);
  });

  it('validates contribution/comment text length', () => {
    expect(validateContributionText('  ')).toEqual({ ok: false, reason: '贡献内容不能为空' });
    expect(validateContributionText('方案 A')).toEqual({ ok: true });
    expect(validateCommentText('')).toEqual({ ok: false, reason: '评论不能为空' });
  });
});

describe('spark-affairs model: timeline sorting', () => {
  it('sorts by declared time then opHash for display only (no cross-replica determinism claim)', () => {
    const mk = (opHash: string, declaredAt: number): AffairOperation => ({
      opHash,
      prevOpHash: '00'.repeat(32),
      kind: 'comment',
      author: 'ab'.repeat(32),
      payload: { text: opHash },
      declaredAt
    });
    // 乱序到达：b 先声明、a 同刻但哈希序在 b 前、d 更早
    const arrival = [mk('b', 100), mk('a', 100), mk('c', 100), mk('d', 90)];
    expect(sortOperations(arrival).map((op) => op.opHash)).toEqual(['d', 'a', 'b', 'c']);
    // 输入顺序无关（同一副本内稳定）
    expect(sortOperations([...arrival].reverse()).map((op) => op.opHash)).toEqual(['d', 'a', 'b', 'c']);
  });
});

describe('spark-affairs model: capability gates and tally', () => {
  it('gates capabilities by ladder level (observer can only comment)', () => {
    expect(canComment()).toBe(true);
    expect(canSubmitContribution('observer')).toBe(false);
    expect(canSubmitContribution('contributor')).toBe(true);
    expect(canSubmitContribution('voter')).toBe(true);
    expect(canVote('observer')).toBe(false);
    expect(canVote('contributor')).toBe(false);
    expect(canVote('voter')).toBe(true);
  });

  it('evaluates tally with quorum gate and pass threshold (abstain excluded)', () => {
    const rules = { passThreshold: 0.67, minQuorum: 3 };
    // 2/3 ≈ 0.667 < 0.67：未达阈值
    expect(evaluateTally({ for: 2, against: 1, abstain: 5 }, rules)).toBe('rejected');
    expect(evaluateTally({ for: 3, against: 0, abstain: 2 }, rules)).toBe('passed');
    expect(evaluateTally({ for: 1, against: 0, abstain: 0 }, rules)).toBe('rejected');
  });

  it('builds app message summary with mandatory prefix', () => {
    expect(buildAffairSummary('绿植补种预算表决')).toBe('【新议题】绿植补种预算表决');
  });
});

describe('spark-affairs model: resolution planning (§6 closure)', () => {
  const CONDITION = { type: 'op-count', opType: 'content', filter: 'vote', count: 2 };
  const RULES = { passThreshold: 0.67, minQuorum: 2 };
  const RULES_HASH = 'ef'.repeat(32);
  const PUB_MS = 24 * 3600 * 1000;
  const VOTERS = ['aa'.repeat(32), 'bb'.repeat(32), 'cc'.repeat(32)];

  function voteOp(opHash: string, actor: string, choice: string, prevOpHash: string) {
    return {
      opHash,
      op: {
        opV: 1,
        affairId: 'af',
        opType: 'content',
        prevOpHash,
        payload: { kind: 'vote', targetOpHash: 'dd'.repeat(32), choice, identityMode: 'contextual' },
        actor: { kind: 'person', identity: actor, publicKey: 'k' },
        declaredAt: 1
      } as Record<string, unknown>
    };
  }

  function planWith(ops: Array<{ opHash: string; op: Record<string, unknown> }>, heads: string[]) {
    return planResolution({
      ops,
      heads,
      closeConditions: [CONDITION],
      voters: VOTERS,
      rules: RULES,
      rulesHash: RULES_HASH,
      pubPeriodMs: PUB_MS
    });
  }

  it('returns a kernel-recomputable plan when the vote quorum is met', () => {
    // 线性链：v1 ← v2 ← c9（opHash 升序 11 < 22 < 99）；heads 只有一个
    const ops = [
      voteOp('11'.repeat(32), VOTERS[0], 'for', 'af'),
      voteOp('22'.repeat(32), VOTERS[1], 'for', '11'.repeat(32)),
      {
        opHash: '99'.repeat(32),
        op: {
          opV: 1, affairId: 'af', opType: 'content', prevOpHash: '22'.repeat(32),
          payload: { kind: 'comment', text: '附议' },
          actor: { kind: 'person', identity: VOTERS[2], publicKey: 'k' }, declaredAt: 2
        } as Record<string, unknown>
      }
    ];
    const plan = planWith(ops, ['99'.repeat(32)]);
    expect(plan).not.toBeNull();
    // countedOps = 闭包内 vote 操作 opHash 升序前 minQuorum 条（内核 op-count 复算口径；
    // 评论不参与 filter 'vote' 匹配）
    expect(plan?.countedOps).toEqual(['11'.repeat(32), '22'.repeat(32)]);
    expect(plan?.condition).toEqual(CONDITION);
    expect(plan?.prevOpHash).toBe('99'.repeat(32));
    expect(plan?.tally).toEqual({ for: 2, against: 0, abstain: 0 });
    expect(plan?.result).toBe('passed'); // 2/2 = 1 ≥ 0.67
    expect(plan?.rulesHash).toBe(RULES_HASH);
    expect(plan?.pubPeriodMs).toBe(PUB_MS);
  });

  it('returns null when votes are below quorum or no decidable condition is declared', () => {
    const oneVote = [voteOp('11'.repeat(32), VOTERS[0], 'for', 'af')];
    expect(planWith(oneVote, ['11'.repeat(32)])).toBeNull();

    // 旧创世 closeConditions=[] 的议题：诚实返回 null（不可伪造条件）
    const twoVotes = [
      voteOp('11'.repeat(32), VOTERS[0], 'for', 'af'),
      voteOp('22'.repeat(32), VOTERS[1], 'for', '11'.repeat(32))
    ];
    expect(
      planResolution({
        ops: twoVotes,
        heads: ['22'.repeat(32)],
        closeConditions: [],
        voters: VOTERS,
        rules: RULES,
        rulesHash: RULES_HASH,
        pubPeriodMs: PUB_MS
      })
    ).toBeNull();
  });

  it('keeps kernel gate on raw vote ops while tally filters roster + dedupes per actor', () => {
    const outsider = 'dd'.repeat(32); // 非投票者名册成员
    const ops = [
      voteOp('11'.repeat(32), outsider, 'for', 'af'), // 非名册票：计入内核门槛，不计 tally
      voteOp('22'.repeat(32), VOTERS[0], 'against', '11'.repeat(32)),
      voteOp('33'.repeat(32), VOTERS[0], 'for', '22'.repeat(32)) // 同一人第二票：tally 不重复计
    ];
    const plan = planWith(ops, ['33'.repeat(32)]);
    // 内核门槛按原始 vote 操作计数（不按人去重）：3 ≥ 2 满足
    expect(plan?.countedOps).toEqual(['11'.repeat(32), '22'.repeat(32)]);
    // tally 只计名册内一人一票（取 opHash 最小的有效票：VOTERS[0] 的 22 票）
    expect(plan?.tally).toEqual({ for: 0, against: 1, abstain: 0 });
    expect(plan?.result).toBe('rejected');
  });

  it('counts only votes inside the resolution causal closure (side branch excluded)', () => {
    // 主链：v1 ← c9；侧分支：v2（prev 同为 af，不在所选头的祖先链上）
    const ops = [
      voteOp('11'.repeat(32), VOTERS[0], 'for', 'af'),
      voteOp('22'.repeat(32), VOTERS[1], 'for', 'af'),
      {
        opHash: '99'.repeat(32),
        op: {
          opV: 1, affairId: 'af', opType: 'content', prevOpHash: '11'.repeat(32),
          payload: { kind: 'comment', text: 'x' },
          actor: { kind: 'person', identity: VOTERS[2], publicKey: 'k' }, declaredAt: 2
        } as Record<string, unknown>
      }
    ];
    // 头取 opHash 最小者 11：闭包只含 11（99 分支上的评论不算 vote；22 不在 11 的祖先链）
    const plan = planWith(ops, ['99'.repeat(32), '11'.repeat(32)]);
    expect(plan).toBeNull();
    // 头为 99 的副本视角：闭包 11←99 只含 1 条 vote，仍不足法定人数
    expect(planWith(ops, ['99'.repeat(32)])).toBeNull();
  });
});

describe('spark-affairs model: SDK shape → view mapping', () => {
  const genesis = {
    affairV: 1,
    type: 'spark-affairs:topic',
    title: '绿植补种',
    summary: '简介',
    tags: ['预算'],
    initiator: { kind: 'person', identity: 'ab'.repeat(32), publicKey: 'pk' },
    createdAt: 1_700_000_000_000,
    rules: {
      engine: 'b1',
      pubPeriod: { delayMs: 48 * 3600 * 1000, vetoThreshold: { count: 1 } },
      ruleChange: { kind: 'delayed-veto', delayMs: 48 * 3600 * 1000, vetoThreshold: { count: 1 } },
      exec: null,
      sparkAffairs: { passThreshold: 0.5, minQuorum: 2 }
    }
  };

  it('reads genesis meta and rules; rejects malformed shapes', () => {
    expect(readGenesisMeta(genesis)).toEqual({
      title: '绿植补种',
      summary: '简介',
      tags: ['预算'],
      refs: [],
      originator: 'ab'.repeat(32),
      createdAt: 1_700_000_000_000
    });
    // 创世 refs 原文透传到视图（§10；形状不全的条目跳过不展示）
    const withRefs = {
      ...genesis,
      refs: [{ target: 'cd'.repeat(32), rel: 'inherit' }, { target: 'bad' }]
    };
    expect(readGenesisMeta(withRefs)?.refs).toEqual([{ target: 'cd'.repeat(32), rel: 'inherit' }]);
    expect(readGenesisMeta(null)).toBeNull();
    expect(readGenesisMeta({ title: 'x' })).toBeNull();

    const rules = rulesFromGenesis(genesis);
    expect(rules.reviewPeriodHours).toBe(48);
    expect(rules.passThreshold).toBe(0.5);
    expect(rules.minQuorum).toBe(2);
    expect(rules.entryRequirement).toEqual({ kind: 'none' });
    // 插件语义参数缺席时回退产品默认值
    const noParams = { ...genesis, rules: { engine: 'b1' } };
    expect(rulesFromGenesis(noParams).passThreshold).toBe(0.67);
    expect(rulesFromGenesis(noParams).reviewPeriodHours).toBe(24);
  });

  it('maps readLog entries to timeline view; skips non-plugin kinds and malformed entries', () => {
    const op = {
      opV: 1,
      affairId: 'cd'.repeat(32),
      opType: 'content',
      prevOpHash: 'cd'.repeat(32),
      payload: { kind: 'contribution', text: '议案' },
      actor: { kind: 'person', identity: 'ab'.repeat(32), publicKey: 'pk' },
      declaredAt: 1000,
      sig: 'sig'
    };
    expect(toAffairOperation('hash-1', op)).toEqual({
      opHash: 'hash-1',
      prevOpHash: 'cd'.repeat(32),
      author: 'ab'.repeat(32),
      declaredAt: 1000,
      kind: 'contribution',
      payload: { text: '议案' }
    });
    // 内核级操作（resolution 等）不进客户端时间线
    expect(toAffairOperation('h', { ...op, payload: { result: {} } })).toBeNull();
    expect(toAffairOperation('h', { ...op, declaredAt: 'soon' })).toBeNull();
  });

  it('maps resolution entries with honest unanchored state', () => {
    expect(
      toResolutionView({
        opHash: 'h1',
        result: { adopted: 'op-x' },
        condition: {},
        countedOps: [],
        rulesHash: 'rh',
        pubPeriodMs: 86_400_000,
        anchoredMs: null,
        objections: 2,
        state: 'unanchored'
      })
    ).toEqual({ opHash: 'h1', state: 'unanchored', resultText: '{"adopted":"op-x"}', objections: 2, anchoredMs: null, pubPeriodMs: 86_400_000 });
    expect(toResolutionView({ opHash: 'h1', state: 'bogus' })).toBeNull();
  });

  it('maps ladder entries to per-participant view (decay warning at 90d)', () => {
    const entry = {
      identity: 'ab'.repeat(32),
      tier: 'voter',
      accepts: 4,
      tierSinceMs: 0,
      lastActivityMs: 0,
      accountAgeMs: 120 * 24 * 3600 * 1000
    };
    const now = 91 * 24 * 3600 * 1000;
    expect(toLadderState(entry, now)).toEqual({
      level: 'voter',
      accountAgeDays: 120,
      adoptedContributions: 4,
      daysAtCurrentLevel: 91,
      lastActiveDaysAgo: 91,
      decayWarning: true
    });
    expect(toLadderState({ ...entry, tier: 'bogus' }, now)).toBeNull();
  });
});

describe('spark-affairs model: rules chain / exec / org effects mappers', () => {
  it('toRulesChainView maps versions and change fates; malformed shape returns null', () => {
    const view = toRulesChainView({
      affairId: 'af',
      nowMs: 1,
      current: { seq: 2, rulesHash: 'rh-2', rules: {} },
      versions: [
        { seq: 0, basisOpHash: 'af', rulesHash: 'rh-0', effectiveMs: 10 },
        { seq: 1, basisOpHash: 'h1', rulesHash: 'rh-1', effectiveMs: null }
      ],
      changes: [
        { opHash: 'h2', fate: 'pending', reason: 'pub-period-not-elapsed' },
        { opHash: 'h3', fate: 'rejected', reason: 'veto-threshold-reached' },
        { opHash: 'h4', fate: 'bogus', reason: 'x' }
      ]
    });
    expect(view?.currentSeq).toBe(2);
    expect(view?.versions).toHaveLength(2);
    expect(view?.versions[1].effectiveMs).toBeNull();
    // 非法 fate 条目跳过不展示（内核枚举外的形状不进视图）
    expect(view?.changes).toEqual([
      { opHash: 'h2', fate: 'pending', reason: 'pub-period-not-elapsed' },
      { opHash: 'h3', fate: 'rejected', reason: 'veto-threshold-reached' }
    ]);

    expect(toRulesChainView(null)).toBeNull();
    expect(toRulesChainView({ current: {} })).toBeNull();
  });

  it('toExecStateViews returns empty for non-exec affairs and filters unknown states', () => {
    expect(toExecStateViews({ affairId: 'af', exec: null, states: [] })).toEqual([]);
    expect(toExecStateViews(null)).toEqual([]);

    const states = toExecStateViews({
      affairId: 'af',
      exec: { executor: {} },
      states: [
        { resolutionOpHash: 'h1', state: 'verifying', reportOpHash: 'hr', anchoredMs: 1, effectiveMs: 2 },
        { resolutionOpHash: 'h2', state: 'not-a-state', reportOpHash: null, anchoredMs: null, effectiveMs: null }
      ]
    });
    expect(states).toEqual([
      { resolutionOpHash: 'h1', state: 'verifying', reportOpHash: 'hr', anchoredMs: 1, effectiveMs: 2 }
    ]);
  });

  it('toOrgEffectsView maps outcome + receipt state; malformed shape returns null', () => {
    const view = toOrgEffectsView({
      orgId: 'org_x',
      affairId: 'af',
      effects: [
        { scope: 'policy', grantKey: 'k', resolutionOpHash: 'h1', outcome: 'apply', receipt: { state: 'recorded', receiptKey: 'r' } },
        { scope: 'roster', grantKey: 'k2', resolutionOpHash: 'h2', outcome: 'revoked' }
      ],
      invalidResolutions: ['h-bad']
    });
    expect(view?.effects).toEqual([
      { scope: 'policy', resolutionOpHash: 'h1', outcome: 'apply', receiptState: 'recorded' },
      { scope: 'roster', resolutionOpHash: 'h2', outcome: 'revoked', receiptState: null }
    ]);
    expect(view?.invalidResolutions).toEqual(['h-bad']);

    expect(toOrgEffectsView({ orgId: 'org_x' })).toBeNull();
    expect(toOrgEffectsView('nope')).toBeNull();
  });
});
