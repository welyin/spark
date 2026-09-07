import { describe, expect, it, vi } from 'vitest';
import { AffairsService } from '../service';
import { AFFAIRS_MODULE_MISSING, REQUIRED_AFFAIRS_METHODS } from '../sdk-affairs';
import { AFFAIR_TYPE, signPayload } from '../wire';

/**
 * mock SDK：affairs 模块按已落地 SDK 面（plugin-sdk PluginAffairsAPI：
 * create 承载创世线形构造 + 域身份签名 + follow 的组合，插件不再自有
 * 创世构造）实现——钉住真实契约，接口错位会在测试里立刻暴露；
 * identity/messages 对齐 spark-example 的 mock 形态。
 */
const PUB_KEY = '0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=';
const IDENTITY = '10ba682c8ad13513971e8b56881aab8bd702bb807796eca81932c735a94d6e6d';
const AFFAIR_ID = 'ab'.repeat(32);
const ORG_ID = `org_${'cd'.repeat(8)}`;

const GENESIS = {
  affairV: 1,
  type: AFFAIR_TYPE,
  title: '绿植补种',
  summary: '简介',
  tags: [],
  initiator: { kind: 'person', identity: IDENTITY, publicKey: PUB_KEY },
  refs: [],
  createdAt: 1_700_000_000_000,
  rules: { engine: 'b1', ruleChange: { kind: 'delayed-veto', delayMs: 86_400_000, vetoThreshold: { count: 1 } }, exec: null },
  sig: 'sig-1'
};

function createMockSdk() {
  let opCounter = 0;
  const affairs = {
    create: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, genesis: GENESIS }),
    follow: vi.fn().mockResolvedValue(AFFAIR_ID),
    unfollow: vi.fn().mockResolvedValue(undefined),
    listFollowed: vi.fn().mockResolvedValue([]),
    submitOp: vi.fn().mockImplementation((op: { affairId: string }) => {
      opCounter += 1;
      return Promise.resolve({ affairId: op.affairId, opHash: `hash-${opCounter}`, status: 'accepted' });
    }),
    readLog: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, genesis: null, ops: [], heads: [], followedAt: null }),
    readRules: vi.fn().mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: 1_700_000_000_000,
      current: { seq: 0, rulesHash: 'rh-0', rules: GENESIS.rules },
      versions: [{ seq: 0, basisOpHash: AFFAIR_ID, rulesHash: 'rh-0', effectiveMs: 1_699_000_000_000 }],
      changes: []
    }),
    readResolution: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, resolutions: [] }),
    ladderStatus: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, nowMs: 1_700_000_000_000, entries: [], voters: [] }),
    readExec: vi.fn().mockResolvedValue({ affairId: AFFAIR_ID, nowMs: 1_700_000_000_000, exec: null, states: [] }),
    orgEffects: vi.fn().mockResolvedValue({ orgId: ORG_ID, affairId: AFFAIR_ID, nowMs: 1_700_000_000_000, effects: [], invalidResolutions: [] }),
    applyOrgEffects: vi.fn().mockResolvedValue({ orgId: ORG_ID, affairId: AFFAIR_ID, nowMs: 1_700_000_000_000, actions: [], invalidResolutions: [] }),
    onChange: vi.fn().mockResolvedValue(undefined)
  };
  return {
    sdk: {
      affairs,
      identity: {
        sign: vi.fn().mockResolvedValue({
          domain: 'plugin:spark-affairs',
          domainId: 'spark-affairs',
          publicKey: PUB_KEY,
          signature: 'sig-1',
          payloadHash: 'ph-1'
        }),
        verify: vi.fn().mockResolvedValue({ valid: true })
      },
      messages: {
        sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' })
      },
      policy: {
        read: vi.fn().mockResolvedValue(null),
        submitDraft: vi.fn().mockResolvedValue({ ok: true, findings: [] }),
        publish: vi.fn().mockResolvedValue({ ok: true })
      }
    },
    affairs
  };
}

function validDraft() {
  return {
    title: '议题',
    summary: '简介',
    tags: [],
    refs: [] as Array<{ target: string; rel: 'inherit' | 'appeal' | 'parent' | 'related' }>,
    rules: {
      reviewPeriodHours: 24,
      passThreshold: 0.67,
      minQuorum: 1,
      initialVoters: [] as string[],
      entryRequirement: { kind: 'none' as const }
    }
  };
}

describe('spark-affairs service: module docking', () => {
  it('throws AFFAIRS_MODULE_MISSING when host has no sdk.affairs', () => {
    const sdk = createMockSdk().sdk as any;
    delete sdk.affairs;
    expect(() => new AffairsService(sdk)).toThrow(AFFAIRS_MODULE_MISSING);
    expect(AffairsService.isAvailable(sdk)).toBe(false);
  });

  it('fails fast naming the missing method when the module shape is off the landed SDK surface', () => {
    const sdk = createMockSdk().sdk as any;
    delete sdk.affairs.submitOp;
    expect(() => new AffairsService(sdk)).toThrow(/submitOp/);
    expect(AffairsService.isAvailable(sdk)).toBe(false);

    // 全方法齐全才放行（对接核对清单 = REQUIRED_AFFAIRS_METHODS）
    expect(REQUIRED_AFFAIRS_METHODS).toContain('readResolution');
    expect(AffairsService.isAvailable(createMockSdk().sdk as any)).toBe(true);
  });
});

describe('spark-affairs service: creation flow (sdk.affairs.create → submitOp)', () => {
  it('creates affair via SDK-composed create (typed genesis input), opening op via submitOp', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    const created = await service.createAffair(validDraft());

    expect(created.affairId).toBe(AFFAIR_ID);
    // 创世走 sdk.affairs.create 的类型化描述（线形构造 + 签名 + follow 由 SDK 承载，
    // 插件不再自有创世构造；线形断言见 packages/plugin-sdk/tests/affair-wire.test.ts）
    const input = affairs.create.mock.calls[0][0] as Record<string, any>;
    expect(input.type).toBe(AFFAIR_TYPE);
    expect(input.title).toBe('议题');
    expect(input.summary).toBe('简介');
    expect(input.refs).toEqual([]);
    expect(input.rules.engine).toBe('b1');
    expect(input.rules.ruleChange.kind).toBe('delayed-veto');
    // 首条开题说明操作：prevOpHash = affairId（无已知 DAG 头），插件域身份签名
    const op = affairs.submitOp.mock.calls[0][0] as Record<string, any>;
    expect(op.opV).toBe(1);
    expect(op.affairId).toBe(AFFAIR_ID);
    expect(op.opType).toBe('content');
    expect(op.prevOpHash).toBe(AFFAIR_ID);
    expect(op.payload).toEqual({ kind: 'comment', text: '简介' });
    expect(op.actor).toEqual({ kind: 'person', identity: IDENTITY, publicKey: PUB_KEY });
    expect(op.sig).toBe('sig-1');
  });

  it('carries typed refs (§10) into the genesis input verbatim', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await service.createAffair({ ...validDraft(), refs: [{ target: 'cd'.repeat(32), rel: 'inherit' }] });

    const input = affairs.create.mock.calls[0][0] as Record<string, any>;
    expect(input.refs).toEqual([{ target: 'cd'.repeat(32), rel: 'inherit' }]);
  });

  it('rejects invalid draft before touching the SDK', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await expect(service.createAffair({ ...validDraft(), title: '' })).rejects.toThrow(/标题不能为空/);
    expect(affairs.create).not.toHaveBeenCalled();
    expect(sdk.identity.sign).not.toHaveBeenCalled();

    // refs 形状非法同样在签名前拦截（rel 枚举 + 64 hex；自指禁令归内核）
    await expect(
      service.createAffair({ ...validDraft(), refs: [{ target: 'bad', rel: 'inherit' }] })
    ).rejects.toThrow(/64 位小写 hex/);
    expect(affairs.create).not.toHaveBeenCalled();
  });
});

describe('spark-affairs service: operations', () => {
  it('submits contribution as signed content op pointing at the lexicographically smallest head', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.readLog.mockResolvedValue({ affairId: AFFAIR_ID, genesis: null, ops: [], heads: ['ff'.repeat(32), '11'.repeat(32)], followedAt: 1 });
    const service = new AffairsService(sdk as any);

    const result = await service.submitContribution(AFFAIR_ID, '议案正文');

    expect(result).toEqual({ opHash: 'hash-1', status: 'accepted' });
    const op = affairs.submitOp.mock.calls[0][0] as Record<string, any>;
    expect(op.prevOpHash).toBe('11'.repeat(32));
    expect(op.payload).toEqual({ kind: 'contribution', text: '议案正文' });
    // 签名载荷 = canonical(剔除 sig 全文)，协议验签复算比对
    const sansSig = { ...op };
    delete sansSig.sig;
    expect(sdk.identity.sign).toHaveBeenCalledWith(signPayload(sansSig));
  });

  it('fails the operation when identity:sign is rejected (protocol signature is mandatory, no unsigned degrade)', async () => {
    const { sdk, affairs } = createMockSdk();
    sdk.identity.sign.mockRejectedValue(new Error('Access denied: identity:sign rejected by user'));
    const service = new AffairsService(sdk as any);

    await expect(service.submitContribution(AFFAIR_ID, '拒绝签名则失败')).rejects.toThrow(/identity:sign/);
    expect(affairs.submitOp).not.toHaveBeenCalled();
  });

  it('rejects blank contribution before touching the SDK', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await expect(service.submitContribution(AFFAIR_ID, '   ')).rejects.toThrow(/不能为空/);
    expect(affairs.submitOp).not.toHaveBeenCalled();
  });

  it('submits vote carrying target hash, choice and identity mode', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await service.submitVote(AFFAIR_ID, 'cd'.repeat(32), 'for', 'public');

    const op = affairs.submitOp.mock.calls[0][0] as Record<string, any>;
    expect(op.opType).toBe('content');
    expect(op.payload).toEqual({ kind: 'vote', targetOpHash: 'cd'.repeat(32), choice: 'for', identityMode: 'public' });
  });
});

describe('spark-affairs service: follow / read paths', () => {
  it('follow takes the genesis record (self-certifying), never a self-reported id', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await expect(service.followGenesis(GENESIS)).resolves.toBe(AFFAIR_ID);
    expect(affairs.follow).toHaveBeenCalledWith(GENESIS);
    await expect(service.followGenesis('not-an-object')).rejects.toThrow(/JSON 对象/);
    await service.unfollow(AFFAIR_ID);
    expect(affairs.unfollow).toHaveBeenCalledWith(AFFAIR_ID);
  });

  it('lists followed affairs with genesis meta; skips affairs whose genesis has not synced yet', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.listFollowed.mockResolvedValue([AFFAIR_ID, 'cd'.repeat(32)]);
    affairs.readLog.mockImplementation((affairId: string) =>
      Promise.resolve(
        affairId === AFFAIR_ID
          ? { affairId, genesis: GENESIS, ops: [{ opHash: 'h1', op: {} }], heads: ['h1'], followedAt: 100 }
          : { affairId, genesis: null, ops: [], heads: [], followedAt: 200 }
      )
    );
    const service = new AffairsService(sdk as any);

    const items = await service.listFollowed();

    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      affairId: AFFAIR_ID,
      title: '绿植补种',
      originator: IDENTITY,
      following: true,
      operationCount: 1
    });
  });

  it('getDetail marks closed when any resolution is effective/vetoed', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.readLog.mockImplementation((affairId: string) =>
      Promise.resolve(
        affairId === AFFAIR_ID
          ? { affairId, genesis: GENESIS, ops: [], heads: [], followedAt: 100 }
          : { affairId, genesis: null, ops: [], heads: [], followedAt: null }
      )
    );
    affairs.readResolution.mockResolvedValue({
      affairId: AFFAIR_ID,
      resolutions: [
        { opHash: 'h1', result: {}, condition: {}, countedOps: [], rulesHash: 'rh', pubPeriodMs: 86_400_000, anchoredMs: 1, objections: 0, state: 'effective' }
      ]
    });
    const service = new AffairsService(sdk as any);

    const detail = await service.getDetail(AFFAIR_ID);

    expect(detail.closed).toBe(true);
    expect(detail.rules.passThreshold).toBe(0.67); // sparkAffairs 参数缺席 → 产品默认值
    await expect(service.getDetail('unknown')).rejects.toThrow(); // genesis null → 明确报错
  });

  it('getMyLadderState is null before any write (no actor) and maps the roster entry after', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await expect(service.getMyLadderState(AFFAIR_ID)).resolves.toBeNull();

    await service.submitComment(AFFAIR_ID, '先写一条');
    affairs.ladderStatus.mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: 1_700_000_000_000,
      entries: [
        { identity: IDENTITY, tier: 'contributor', accepts: 2, tierSinceMs: 1_699_000_000_000, lastActivityMs: 1_699_900_000_000, accountAgeMs: 30 * 86_400_000 }
      ],
      voters: []
    });
    const mine = await service.getMyLadderState(AFFAIR_ID);
    expect(mine?.level).toBe('contributor');
    expect(mine?.accountAgeDays).toBe(30);
    expect(service.viewerIdentity).toBe(IDENTITY);
  });
});

describe('spark-affairs service: app-session notification (degradable)', () => {
  it('sends app message with mandatory summary and affair-card reference', async () => {
    const { sdk } = createMockSdk();
    const service = new AffairsService(sdk as any);

    await expect(service.notifyNewAffair('绿植补种', AFFAIR_ID)).resolves.toBe(true);

    const [payload, card] = (sdk.messages.sendAppMessage as any).mock.calls[0];
    expect(payload.summary).toBe('【新议题】绿植补种');
    expect(card).toEqual({ viewId: 'affair-card', data: { affairId: AFFAIR_ID } });
  });

  it('degrades notify to false when message:app is denied or unavailable', async () => {
    const { sdk } = createMockSdk();
    sdk.messages.sendAppMessage.mockRejectedValueOnce(new Error('rate-limited'));
    const service = new AffairsService(sdk as any);
    await expect(service.notifyNewAffair('限流', AFFAIR_ID)).resolves.toBe(false);

    const noMessages = createMockSdk().sdk as any;
    delete noMessages.messages;
    const silent = new AffairsService(noMessages);
    await expect(silent.notifyNewAffair('无桥环境', AFFAIR_ID)).resolves.toBe(false);
  });
});

describe('spark-affairs service: rules chain / exec states (read paths)', () => {
  it('maps rules chain versions and un-effected change fates', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.readRules.mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: 1_700_000_000_000,
      current: { seq: 1, rulesHash: 'rh-1', rules: {} },
      versions: [
        { seq: 0, basisOpHash: AFFAIR_ID, rulesHash: 'rh-0', effectiveMs: 1_699_000_000_000 },
        { seq: 1, basisOpHash: 'h-rc', rulesHash: 'rh-1', effectiveMs: 1_699_500_000_000 }
      ],
      changes: [{ opHash: 'h-pend', fate: 'pending', reason: 'pub-period-not-elapsed' }]
    });
    const service = new AffairsService(sdk as any);

    const chain = await service.getRulesChain(AFFAIR_ID);

    expect(chain?.currentSeq).toBe(1);
    expect(chain?.versions).toHaveLength(2);
    expect(chain?.changes).toEqual([{ opHash: 'h-pend', fate: 'pending', reason: 'pub-period-not-elapsed' }]);
  });

  it('returns empty exec states for non-exec affairs and maps the eight-state machine otherwise', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new AffairsService(sdk as any);

    // exec == null（非执行型事务）→ 空列表（决议即终态）
    await expect(service.getExecStates(AFFAIR_ID)).resolves.toEqual([]);

    affairs.readExec.mockResolvedValue({
      affairId: AFFAIR_ID,
      nowMs: 1_700_000_000_000,
      exec: { executor: { kind: 'person', identity: IDENTITY }, verify: { kind: 'vote' } },
      rosterSize: 3,
      states: [
        { resolutionOpHash: 'h-r1', state: 'awaiting-execution', reportOpHash: null, anchoredMs: 1, effectiveMs: 2 },
        { resolutionOpHash: 'h-r2', state: 'closed', reportOpHash: 'h-rep', anchoredMs: 1, effectiveMs: 2 }
      ]
    });
    const states = await service.getExecStates(AFFAIR_ID);
    expect(states.map((s) => s.state)).toEqual(['awaiting-execution', 'closed']);
    expect(states[1].reportOpHash).toBe('h-rep');
  });
});

describe('spark-affairs service: org effects orchestration', () => {
  const POLICY_DOC = { policyV: 1, engine: 'b1', rules: [] };

  function mockPendingEffect(effects: Array<Record<string, unknown>>) {
    return { orgId: ORG_ID, affairId: AFFAIR_ID, nowMs: 1_700_000_000_000, effects, invalidResolutions: [] };
  }

  it('applies policy effect via sdk.policy.submitDraft, then writes the receipt (apply → receipt ordering)', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.orgEffects.mockResolvedValue(
      mockPendingEffect([
        {
          scope: 'policy',
          grantKey: 'k1',
          resolutionOpHash: 'h-res',
          outcome: 'apply',
          pendingEffect: { orgId: ORG_ID, affairId: AFFAIR_ID, resolutionOpHash: 'h-res', scope: 'policy', grantKey: 'k1' },
          receipt: { state: 'unrecorded', receiptKey: 'r1' }
        }
      ])
    );
    affairs.readResolution.mockResolvedValue({
      affairId: AFFAIR_ID,
      resolutions: [{ opHash: 'h-res', result: POLICY_DOC, state: 'effective' }]
    });
    affairs.applyOrgEffects.mockResolvedValue({
      orgId: ORG_ID,
      affairId: AFFAIR_ID,
      nowMs: 1,
      actions: [{ scope: 'policy', resolutionOpHash: 'h-res', action: 'recorded', receiptKey: 'r1' }],
      invalidResolutions: []
    });
    const service = new AffairsService(sdk as any);

    const report = await service.applyOrgEffects(ORG_ID, AFFAIR_ID);

    expect(sdk.policy.submitDraft).toHaveBeenCalledWith(POLICY_DOC);
    expect(affairs.applyOrgEffects).toHaveBeenCalledWith(ORG_ID, AFFAIR_ID);
    expect(report.applied).toEqual(['policy']);
    expect(report.unapplied).toEqual([]);
    expect(report.receiptActions).toEqual([{ scope: 'policy', resolutionOpHash: 'h-res', action: 'recorded' }]);
  });

  it('writes no receipt when a scope has no plugin-side write surface (fail-closed, honest)', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.orgEffects.mockResolvedValue(
      mockPendingEffect([
        { scope: 'roster', grantKey: 'k1', resolutionOpHash: 'h-res', outcome: 'apply', receipt: { state: 'unrecorded', receiptKey: 'r1' } }
      ])
    );
    const service = new AffairsService(sdk as any);

    const report = await service.applyOrgEffects(ORG_ID, AFFAIR_ID);

    expect(report.receiptActions).toBeNull();
    expect(report.unapplied).toHaveLength(1);
    expect(report.unapplied[0].scope).toBe('roster');
    expect(affairs.applyOrgEffects).not.toHaveBeenCalled();
    expect(sdk.policy.submitDraft).not.toHaveBeenCalled();
  });

  it('writes no receipt when policy draft submission is denied (permission degrade, no fabrication)', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.orgEffects.mockResolvedValue(
      mockPendingEffect([
        { scope: 'policy', grantKey: 'k1', resolutionOpHash: 'h-res', outcome: 'apply', receipt: { state: 'unrecorded', receiptKey: 'r1' } }
      ])
    );
    affairs.readResolution.mockResolvedValue({
      affairId: AFFAIR_ID,
      resolutions: [{ opHash: 'h-res', result: POLICY_DOC, state: 'effective' }]
    });
    sdk.policy.submitDraft.mockRejectedValue(new Error('Access denied: policy:write'));
    const service = new AffairsService(sdk as any);

    const report = await service.applyOrgEffects(ORG_ID, AFFAIR_ID);

    expect(report.receiptActions).toBeNull();
    expect(report.unapplied[0].reason).toMatch(/policy:write/);
    expect(affairs.applyOrgEffects).not.toHaveBeenCalled();
  });

  it('no pending effects → early return without touching applyOrgEffects; recorded effects are not reapplied', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.orgEffects.mockResolvedValue(
      mockPendingEffect([
        { scope: 'policy', grantKey: 'k1', resolutionOpHash: 'h-res', outcome: 'apply', receipt: { state: 'recorded', receiptKey: 'r1' } },
        { scope: 'roster', grantKey: 'k2', resolutionOpHash: 'h-res2', outcome: 'notDeclared' }
      ])
    );
    const service = new AffairsService(sdk as any);

    const report = await service.applyOrgEffects(ORG_ID, AFFAIR_ID);

    expect(report).toEqual({ applied: [], unapplied: [], receiptActions: null });
    expect(affairs.applyOrgEffects).not.toHaveBeenCalled();
  });

  it('getOrgEffects maps outcome and receipt state for the UI', async () => {
    const { sdk, affairs } = createMockSdk();
    affairs.orgEffects.mockResolvedValue(
      mockPendingEffect([
        { scope: 'policy', grantKey: 'k1', resolutionOpHash: 'h-res', outcome: 'apply', receipt: { state: 'unrecorded', receiptKey: 'r1' } },
        { scope: 'roster', grantKey: 'k2', resolutionOpHash: 'h-res2', outcome: 'notPrior' }
      ])
    );
    const service = new AffairsService(sdk as any);

    const view = await service.getOrgEffects(ORG_ID, AFFAIR_ID);

    expect(view?.effects).toEqual([
      { scope: 'policy', resolutionOpHash: 'h-res', outcome: 'apply', receiptState: 'unrecorded' },
      { scope: 'roster', resolutionOpHash: 'h-res2', outcome: 'notPrior', receiptState: null }
    ]);
  });
});
