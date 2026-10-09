import { describe, expect, it, vi } from 'vitest';
import { AFFAIRS_MODULE_MISSING, ProjectService, REQUIRED_AFFAIRS_METHODS } from '../service';
import { DISPOSITION_KIND, PROJECT_COMMENT_KIND } from '../model';
import { deriveIdentity } from '../wire';
import { RELEASE_COLLECTIONS } from '../vendor/github.com/welyin/spark/plugins/spark-release-manager/service';
import type { ReleaseEvent } from '../vendor/github.com/welyin/spark/plugins/spark-release-manager/model';

/**
 * mock SDK（对齐 spark-kanban/spark-release-manager 测试口径）：
 * - data 为内存 P6 集合后端：declareCollection/save/get/query/delete，按声明
 *   enforce append-only，且 enforce 集合名前缀 == 'spark-project:'（内核
 *   plugindata NamePrefixMismatch 防线——钉住组合件命名空间纪律：vendor 库
 *   件经 namespace='spark-project' 构造后，其 sdk.data 写面也必须落在本插件
 *   命名空间）；
 * - docs 为内存集合后端（发布管理库件走 sdk.docs，内核按调用方域隔离，集合
 *   名不带前缀），按声明 enforce append-only；
 * - affairs 为内存事务副本（create/follow/readLog/submitOp/readRules/
 *   readResolution/ladderStatus/publicProfile/onChange），钉住真实契约；
 * - identity/messages/market/evidence 按已落地 SDK 面实现。
 */
const PLUGIN_PK = 'cGsx';
const ACTOR_IDENTITY = deriveIdentity(PLUGIN_PK);
const PROJECT_ID = 'c'.repeat(64);
const CHILD_BUG = 'b'.repeat(64);
const CHILD_PROPOSAL = 'd'.repeat(64);
const CHILD_PR = '0f'.repeat(32);
const CHILD_EPIC = 'e'.repeat(64);
const OUTSIDER = 'f'.repeat(64);
const BUNDLE_CID = 'a1'.repeat(32);
const SHA_A = 'a'.repeat(64);
const ADMIN = 'e'.repeat(64);

/** 处置关闭条件（与子事务规则文档 closeConditions 逐字一致） */
const DISPOSITION_CONDITION = { type: 'op-count', opType: 'content', filter: 'project.disposition', count: 1 };

type StoredAffair = {
  genesis: Record<string, unknown> | null;
  ops: Array<{ opHash: string; op: Record<string, unknown> }>;
  followedAt: number | null;
};

type MockOptions = {
  withAffairs?: boolean;
  withMarket?: boolean;
  signError?: boolean;
  messageError?: boolean;
  /** 项目议题规则文档写权集合（缺省 = 本插件域身份） */
  writeSet?: string[];
  /** readRules 整体不可用（规则未同步场景） */
  rulesError?: boolean;
  /** 注入写权集合外 actor 的伪造处置动议 + 伪造决议操作（因果上挂在真实头之后 + 未来 declaredAt） */
  forgedDisposition?: boolean;
  /** submitOp 全部失败（模拟网络/内核暂存不可用，验证部分成功态 S2） */
  submitOpError?: boolean;
  /** 模拟并发写竞态：首次 docs 集合写入被竞对抢占同键（append-only 冲突，验证 S3 重试） */
  raceDocSave?: boolean;
};

function createMockSdk(options: MockOptions = {}) {
  const dataDocs = new Map<string, unknown>();
  const collectionMeta = new Map<string, { merge: string; scope: string }>();
  const docsStore = new Map<string, Record<string, unknown>>();
  const docsSchemas = new Map<string, { syncStrategy: string }>();
  const submittedOps: Array<Record<string, unknown>> = [];
  const createdGenesisInputs: Array<Record<string, unknown>> = [];
  const sentMessages: Array<{ payload: Record<string, unknown>; card?: unknown }> = [];

  const genesisOf = (type: string, title: string, createdAt: number, parentTarget?: string): Record<string, unknown> => ({
    affairV: 1,
    type,
    title,
    summary: `${title} 摘要`,
    tags: [],
    initiator: { kind: 'person', identity: OUTSIDER, publicKey: PLUGIN_PK },
    refs: parentTarget ? [{ target: parentTarget, rel: 'parent' }] : [],
    createdAt,
    rules: {
      maintainers: [ACTOR_IDENTITY],
      closeConditions: [DISPOSITION_CONDITION],
      pubPeriod: { delayMs: 86400000, vetoThreshold: { count: 1 } }
    }
  });

  const affairsStore = new Map<string, StoredAffair>();
  affairsStore.set(PROJECT_ID, { genesis: genesisOf('project', '星火项目', 300), ops: [], followedAt: 1 });
  affairsStore.set(CHILD_BUG, { genesis: genesisOf('bug', '启动崩溃', 100, PROJECT_ID), ops: [], followedAt: 1 });
  const proposalOps: Array<{ opHash: string; op: Record<string, unknown> }> = [
    // 处置动议（content，维护者签名）
    {
      opHash: 'aa',
      op: {
        opV: 1,
        affairId: CHILD_PROPOSAL,
        opType: 'content',
        prevOpHash: CHILD_PROPOSAL,
        payload: { kind: DISPOSITION_KIND, action: 'adopted', note: '好建议' },
        actor: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PLUGIN_PK },
        declaredAt: 50
      }
    },
    // 处置决议（R1：opType=resolution，countedOps 回引动议，入公示期）
    {
      opHash: 'rr',
      op: {
        opV: 1,
        affairId: CHILD_PROPOSAL,
        opType: 'resolution',
        prevOpHash: 'aa',
        payload: {
          result: 'adopted',
          condition: DISPOSITION_CONDITION,
          countedOps: ['aa'],
          rulesHash: 'rh',
          pubPeriod: { delayMs: 86400000 }
        },
        actor: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PLUGIN_PK },
        declaredAt: 60
      }
    }
  ];
  if (options.forgedDisposition) {
    // 伪造：集合外 actor 的动议 + 决议（因果上挂在真实头之后）——读侧必须都不采纳
    proposalOps.push({
      opHash: 'zz',
      op: {
        opV: 1,
        affairId: CHILD_PROPOSAL,
        opType: 'content',
        prevOpHash: 'rr',
        payload: { kind: DISPOSITION_KIND, action: 'closed' },
        actor: { kind: 'person', identity: OUTSIDER, publicKey: PLUGIN_PK },
        declaredAt: Number.MAX_SAFE_INTEGER
      }
    });
    proposalOps.push({
      opHash: 'zy',
      op: {
        opV: 1,
        affairId: CHILD_PROPOSAL,
        opType: 'resolution',
        prevOpHash: 'zz',
        payload: {
          result: 'closed',
          condition: DISPOSITION_CONDITION,
          countedOps: ['zz'],
          rulesHash: 'rh',
          pubPeriod: { delayMs: 86400000 }
        },
        actor: { kind: 'person', identity: OUTSIDER, publicKey: PLUGIN_PK },
        declaredAt: Number.MAX_SAFE_INTEGER
      }
    });
  }
  affairsStore.set(CHILD_PROPOSAL, {
    genesis: genesisOf('proposal', '深色模式', 200, PROJECT_ID),
    ops: proposalOps,
    followedAt: 1
  });
  affairsStore.set(CHILD_PR, { genesis: genesisOf('pr', '修复崩溃的 PR', 175, PROJECT_ID), ops: [], followedAt: 1 });
  affairsStore.set(CHILD_EPIC, { genesis: genesisOf('epic', '未预设类型', 150, PROJECT_ID), ops: [], followedAt: 1 });

  // 决议公示期状态（内核职责；mock 用可翻转变量模拟 pending → effective）
  let resolutionState: 'pending' | 'effective' = 'pending';
  let docSaveRaced = false;

  let createCounter = 0;
  let opCounter = 0;
  const headsOf = (affairId: string, stored: StoredAffair): string[] =>
    stored.ops.length > 0 ? [stored.ops[stored.ops.length - 1].opHash] : [affairId];

  const affairs =
    options.withAffairs === false
      ? undefined
      : {
          create: vi.fn().mockImplementation(async (input: Record<string, unknown>) => {
            createCounter += 1;
            createdGenesisInputs.push(input);
            const affairId = String(createCounter).padStart(64, '0');
            const genesis: Record<string, unknown> = {
              affairV: 1,
              type: input.type,
              title: input.title,
              summary: input.summary,
              tags: input.tags ?? [],
              initiator: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PLUGIN_PK },
              refs: input.refs ?? [],
              createdAt: 1000 + createCounter,
              rules: input.rules,
              ...(input.extra as Record<string, unknown> | undefined),
              ...(input.publish === true ? { publish: true } : {})
            };
            affairsStore.set(affairId, { genesis, ops: [], followedAt: 1000 + createCounter });
            return { affairId, genesis };
          }),
          follow: vi.fn().mockImplementation(async (genesis: Record<string, unknown>) => {
            const affairId = 'f'.repeat(64);
            affairsStore.set(affairId, { genesis, ops: [], followedAt: 2000 });
            return affairId;
          }),
          unfollow: vi.fn().mockImplementation(async (affairId: string) => {
            const stored = affairsStore.get(affairId);
            if (stored) {
              stored.followedAt = null;
            }
          }),
          listFollowed: vi.fn().mockImplementation(async () =>
            [...affairsStore.entries()]
              .filter(([, stored]) => stored.followedAt !== null)
              .map(([affairId]) => affairId)
              .sort()
          ),
          readLog: vi.fn().mockImplementation(async (affairId: string) => {
            const stored = affairsStore.get(affairId);
            if (!stored) {
              return { affairId, genesis: null, ops: [], heads: [affairId], followedAt: null };
            }
            return { affairId, genesis: stored.genesis, ops: stored.ops, heads: headsOf(affairId, stored), followedAt: stored.followedAt };
          }),
          submitOp: vi.fn().mockImplementation(async (op: Record<string, unknown>) => {
            if (options.submitOpError) {
              throw new Error('内核暂存不可用');
            }
            opCounter += 1;
            const opHash = `op-${opCounter}`;
            const stored = affairsStore.get(op.affairId as string);
            stored?.ops.push({ opHash, op });
            submittedOps.push(op);
            return { affairId: op.affairId, opHash, status: 'accepted' };
          }),
          readResolution: vi.fn().mockImplementation(async (affairId: string) => {
            const stored = affairsStore.get(affairId);
            // 动态推导（贴近内核语义）：日志中的 resolution 操作 → 决议条目
            const resolutions = (stored?.ops ?? [])
              .filter((entry) => entry.op.opType === 'resolution')
              .map((entry) => ({
                opHash: entry.opHash,
                state: resolutionState,
                result: (entry.op.payload as Record<string, unknown>)?.result ?? null,
                condition: null,
                countedOps: null,
                rulesHash: 'rh',
                pubPeriodMs: 86400000,
                anchoredMs: 1,
                objections: 0
              }));
            return { affairId, resolutions };
          }),
          readRules: vi.fn().mockImplementation(async (affairId: string) => {
            if (options.rulesError) {
              throw new Error('规则文档尚未同步到本机');
            }
            const stored = affairsStore.get(affairId);
            const genesisRules = (stored?.genesis?.rules ?? {}) as Record<string, unknown>;
            const rules =
              affairId === PROJECT_ID
                ? { ...genesisRules, maintainers: options.writeSet ?? [ACTOR_IDENTITY] }
                : genesisRules;
            return { affairId, nowMs: 1000, current: { seq: 0, rulesHash: 'rh', rules }, versions: [], changes: [] };
          }),
          ladderStatus: vi.fn().mockImplementation(async (affairId: string) => ({
            affairId,
            nowMs: 100_000,
            entries: [
              { identity: ACTOR_IDENTITY, tier: 'contributor', accepts: 2, accountAgeMs: 3 * 86400000, lastActivityMs: 999 },
              { identity: OUTSIDER, tier: 'observer', accepts: 0, accountAgeMs: null, lastActivityMs: null }
            ],
            voters: []
          })),
          publicProfile: vi.fn().mockImplementation(async (identity: string) => ({
            identity,
            nowMs: 100_000,
            affairsParticipated: 1,
            firstActivityMs: 1,
            accountAgeMs: 99_999,
            proposals: 1,
            adoptions: 0,
            votes: 0,
            votesYes: 0,
            votesNo: 0,
            perAffair: [],
            voteHistory: []
          })),
          onChange: vi.fn().mockResolvedValue(undefined)
        };

  const sdk = {
    domain: 'plugin:spark-project',
    affairs,
    identity: {
      sign: vi.fn().mockImplementation(async () => {
        if (options.signError) {
          throw new Error('用户拒绝签名授权');
        }
        return { domain: 'plugin:spark-project', domainId: 'spark-project', publicKey: PLUGIN_PK, signature: 'sig-1', payloadHash: 'h' };
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    data: {
      declareCollection: vi.fn().mockImplementation(async (decl: { name: string; merge?: string; scope?: string }) => {
        // 内核 plugindata 强制：集合名前缀 == 调用方插件 id（组合件命名空间纪律的mock 防线）
        if (!decl.name.startsWith('spark-project:')) {
          throw new Error(`NamePrefixMismatch: collection ${decl.name} must be prefixed with spark-project:（mock enforce）`);
        }
        collectionMeta.set(decl.name, { merge: decl.merge ?? 'lww-record', scope: decl.scope ?? 'sync' });
        return {};
      }),
      get: vi.fn().mockImplementation(async (name: string, key: string) => dataDocs.get(`${name}/${key}`) ?? null),
      save: vi.fn().mockImplementation(async (name: string, key: string, value: unknown) => {
        if (options.raceDocSave && name === 'spark-project:docs' && !docSaveRaced) {
          docSaveRaced = true;
          // 竞对抢先写入同 seq 键（append-only 冲突现场）
          dataDocs.set(`${name}/${key}`, { ...(value as Record<string, unknown>), body: 'concurrent-write' });
          throw new Error(`AppendOnlyViolation: save-overwrite rejected on ${name}（mock enforce）`);
        }
        if (collectionMeta.get(name)?.merge === 'append-only' && dataDocs.has(`${name}/${key}`)) {
          throw new Error(`AppendOnlyViolation: save-overwrite rejected on ${name}（mock enforce）`);
        }
        dataDocs.set(`${name}/${key}`, value);
        return { success: true };
      }),
      delete: vi.fn().mockImplementation(async (name: string, key: string) => {
        if (collectionMeta.get(name)?.merge === 'append-only' && dataDocs.has(`${name}/${key}`)) {
          throw new Error(`AppendOnlyViolation: delete rejected on ${name}（mock enforce）`);
        }
        dataDocs.delete(`${name}/${key}`);
        return { success: true };
      }),
      query: vi.fn().mockImplementation(async (name: string, opts?: { prefix?: string }) => ({
        items: [...dataDocs.entries()]
          .filter(([key]) => key.startsWith(`${name}/${opts?.prefix ?? ''}`))
          .map(([key, value]) => ({ key: key.slice(name.length + 1), value }))
      })),
      onChange: vi.fn().mockResolvedValue(undefined)
    },
    docs: {
      defineCollection: vi.fn().mockImplementation(async (collection: string, schema: { syncStrategy: string }) => {
        docsSchemas.set(collection, schema);
        return { collection, syncStrategy: schema.syncStrategy, governance: false, enableEvidence: true };
      }),
      put: vi.fn().mockImplementation(async (collection: string, id: string, doc: Record<string, unknown>) => {
        if (docsSchemas.get(collection)?.syncStrategy === 'append-only' && docsStore.has(`${collection}/${id}`)) {
          throw new Error(`AppendOnlyViolation: put-overwrite rejected on ${collection}（mock enforce）`);
        }
        docsStore.set(`${collection}/${id}`, doc);
        return { success: true };
      }),
      get: vi.fn().mockImplementation(async (collection: string, id: string) => docsStore.get(`${collection}/${id}`) ?? null),
      delete: vi.fn().mockImplementation(async (collection: string, id: string) => {
        if (docsSchemas.get(collection)?.syncStrategy === 'append-only') {
          throw new Error(`AppendOnlyViolation: delete rejected on ${collection}（mock enforce）`);
        }
        docsStore.delete(`${collection}/${id}`);
        return { success: true };
      }),
      query: vi.fn().mockImplementation(async (collection: string, opts?: { filter?: Array<{ field: string; value: unknown }> }) => ({
        items: [...docsStore.entries()]
          .filter(([key]) => key.startsWith(`${collection}/`))
          .map(([key, data]) => ({ id: key.slice(collection.length + 1), data }))
          .filter((item) => (opts?.filter ?? []).every((f) => (item.data as Record<string, unknown>)[f.field] === f.value))
      }))
    },
    messages: {
      sendAppMessage: vi.fn().mockImplementation(async (payload: Record<string, unknown>, card?: unknown) => {
        if (options.messageError) {
          throw new Error('rate-limited');
        }
        sentMessages.push({ payload, card });
        return { id: `m-${sentMessages.length}` };
      }),
      onCardAction: vi.fn()
    },
    evidence: {
      headHash: vi.fn().mockResolvedValue({ hash: 'ab'.repeat(32) }),
      verify: vi.fn().mockResolvedValue({ valid: true, height: 42 })
    },
    market:
      options.withMarket === false
        ? undefined
        : {
            inspectLocal: vi.fn().mockImplementation(async (path: string) => ({
              pluginId: 'spark-foo',
              domain: 'plugin:spark-foo',
              version: '0.1.0',
              name: 'Foo',
              permissions: [],
              sha256: SHA_A,
              size: 1024,
              fileName: path.split(/[\\/]/).pop()
            })),
            list: vi.fn().mockResolvedValue([]),
            checkUpdates: vi.fn().mockResolvedValue([]),
            pickSpkg: vi.fn().mockResolvedValue('/tmp/x.spkg')
          }
  } as any;

  return {
    sdk,
    dataDocs,
    collectionMeta,
    docsStore,
    docsSchemas,
    submittedOps,
    createdGenesisInputs,
    sentMessages,
    affairsStore,
    setResolutionState: (state: 'pending' | 'effective') => {
      resolutionState = state;
    }
  };
}

const UPDATE_MANIFEST = JSON.stringify({
  assets: [{ fileName: 'spark-plugin-spark-foo-0.1.0.spkg', sha256: SHA_A, size: 1024, url: 'https://example.invalid/x.spkg' }]
});

describe('spark-project service · 集合声明与组合件命名空间纪律（档二-3）', () => {
  it('declares all four collections with designed merge/scope before writing（§3.2）', async () => {
    const { sdk, collectionMeta } = createMockSdk();
    const service = new ProjectService(sdk);
    await service.saveDoc(PROJECT_ID, { title: '设计稿', body: 'v1' });
    expect([...collectionMeta.entries()].sort()).toEqual([
      ['spark-project:docs', { merge: 'append-only', scope: 'sync' }],
      ['spark-project:drafts', { merge: 'lww-record', scope: 'local' }],
      ['spark-project:notified', { merge: 'lww-record', scope: 'local' }],
      ['spark-project:workspace', { merge: 'lww-record', scope: 'sync' }]
    ]);
    // 声明幂等：第二次写入不重复声明
    await service.saveDoc(PROJECT_ID, { title: '设计稿', body: 'v2' });
    expect(sdk.data.declareCollection).toHaveBeenCalledTimes(4);
  });

  it('vendor 库件以 namespace=spark-project 调用：看板/送达台账集合全部落本插件命名空间（mock enforce 前缀）', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    // 看板组合（任一越前缀声明都会被 mock 拒绝）
    const init = await service.initProjectBoard(PROJECT_ID, 'org-1', 'root-admin', 'admin', '星火项目');
    expect(init.ok).toBe(true);
    // 发布组合：送达台账经 sdk.data，须带组合者前缀
    await service.initReleaseConfig('org-1', ADMIN, 'admin');
    const release = await service.registerRelease('org-1', ADMIN, {
      pluginId: 'spark-foo',
      version: '0.1.0',
      updateManifestJson: UPDATE_MANIFEST
    });
    await service.verifyRelease('org-1', ADMIN, release.id, '/tmp/x.spkg');
    await service.publishRelease('org-1', ADMIN, release.id); // 即时推卡片 → 送达台账声明
    const declared = sdk.data.declareCollection.mock.calls.map((call: any[]) => call[0].name);
    expect(declared).toContain('spark-project:boards');
    expect(declared).toContain('spark-project:card-ops');
    expect(declared).toContain('spark-project:bindings');
    expect(declared).toContain('spark-project:delivery');
    expect(declared.every((name: string) => name.startsWith('spark-project:'))).toBe(true);
    // 发布件业务集合走 sdk.docs（内核按调用方域隔离，不带前缀）
    expect(sdk.docs.defineCollection.mock.calls.map((call: any[]) => call[0])).toContain(RELEASE_COLLECTIONS.releases);
  });
});

describe('spark-project service · 创建项目议题（publish 确认门控，档二-2 补录）', () => {
  it('publish:true 未经显式确认即 fail-closed 拒绝（服务层纵深复核，不靠视图层纪律）', async () => {
    const { sdk, createdGenesisInputs, submittedOps } = createMockSdk();
    const service = new ProjectService(sdk);
    await expect(
      service.createProject({ title: '星火', summary: '自举', tags: [], publish: true })
    ).rejects.toThrow('洪泛');
    expect(createdGenesisInputs).toHaveLength(0);
    expect(submittedOps).toHaveLength(0);
    await expect(
      service.createProject({ title: '星火', summary: '自举', tags: [], publish: true }, { confirmedPublish: false })
    ).rejects.toThrow('显式确认');
  });

  it('显式确认后创建：publish 位入创世；maintainers 初始 = 插件域身份 id；首条发言与回执卡片', async () => {
    const { sdk, createdGenesisInputs, submittedOps, sentMessages } = createMockSdk();
    const service = new ProjectService(sdk);
    const { affairId, commentPosted, cardSent } = await service.createProject(
      { title: ' 星火 ', summary: ' 自举项目 ', tags: ['core'], publish: true },
      { confirmedPublish: true }
    );
    expect(commentPosted).toBe(true);
    expect(createdGenesisInputs).toHaveLength(1);
    const genesis = createdGenesisInputs[0];
    expect(genesis.type).toBe('project');
    expect(genesis.publish).toBe(true);
    expect((genesis.rules as Record<string, unknown>).maintainers).toEqual([ACTOR_IDENTITY]);
    // 议题说明作为首条发言入日志（签名 + prevOpHash = affairId）
    expect(submittedOps).toHaveLength(1);
    expect(submittedOps[0]).toMatchObject({ affairId, opType: 'content', prevOpHash: affairId });
    expect((submittedOps[0].payload as Record<string, unknown>).kind).toBe(PROJECT_COMMENT_KIND);
    expect(typeof submittedOps[0].sig).toBe('string');
    expect(cardSent).toBe(true);
    expect(sentMessages[0].card).toMatchObject({ viewId: 'affair-card', data: { kind: 'project-created', affairId } });
    expect(service.viewerIdentity).toBe(ACTOR_IDENTITY);
  });

  it('publish:false 不携带声明位（canonical 最小承诺）', async () => {
    const { sdk, createdGenesisInputs } = createMockSdk();
    const service = new ProjectService(sdk);
    await service.createProject({ title: 't', summary: 's', tags: [] , publish: false });
    expect('publish' in createdGenesisInputs[0]).toBe(false);
  });

  it('创建后段失败降格为部分成功态（S2）：首条发言未写入不阻断，议题本体已成立', async () => {
    const { sdk, createdGenesisInputs, sentMessages } = createMockSdk({ submitOpError: true });
    const service = new ProjectService(sdk);
    const result = await service.createProject({ title: 't', summary: 's', tags: [], publish: false });
    expect(createdGenesisInputs).toHaveLength(1); // 议题本体已成立
    expect(result.affairId).toMatch(/^[0-9a-f]{64}$/);
    expect(result.commentPosted).toBe(false); // 如实标注，不抛错回滚
    expect(result.cardSent).toBe(true); // 卡片通道独立降级
    expect(sentMessages).toHaveLength(1);
  });

  it('输入非法 / 签名被拒 → 失败不降级（签名是内核入站硬要求）', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    await expect(service.createProject({ title: ' ', summary: 's', tags: [], publish: false })).rejects.toThrow('标题');
    const denied = createMockSdk({ signError: true });
    await expect(
      new ProjectService(denied.sdk).createProject({ title: 't', summary: 's', tags: [], publish: false })
    ).rejects.toThrow('签名');
  });

  it('REQUIRED_AFFAIRS_METHODS 钉住本插件依赖的事务面', () => {
    expect([...REQUIRED_AFFAIRS_METHODS]).toEqual(
      expect.arrayContaining(['create', 'follow', 'unfollow', 'listFollowed', 'readLog', 'submitOp', 'readResolution', 'readRules', 'ladderStatus', 'publicProfile', 'onChange'])
    );
  });
});

describe('spark-project service · 关注 / 列表', () => {
  it('listProjects 只列 type=project 的已关注议题（子事务不混入），按创建时刻倒序', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    const projects = await service.listProjects();
    expect(projects).toHaveLength(1);
    expect(projects[0]).toMatchObject({ affairId: PROJECT_ID, type: 'project', title: '星火项目', following: true });
  });

  it('followGenesis 拒绝非对象输入；unfollow 后不再列出', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    await expect(service.followGenesis(null)).rejects.toThrow('JSON 对象');
    await expect(service.followGenesis([1])).rejects.toThrow('JSON 对象');
    const affairId = await service.followGenesis({ affairV: 1, type: 'project', title: '外部议题' });
    expect(affairId).toMatch(/^[0-9a-f]{64}$/);
    expect((await service.listProjects()).map((p) => p.affairId)).toContain(affairId);
    await service.unfollow(PROJECT_ID);
    expect((await service.listProjects()).map((p) => p.affairId)).not.toContain(PROJECT_ID);
  });
});

describe('spark-project service · 子事务聚合与发起（档一-1 反馈回流同路径）', () => {
  it('自动聚合：refs rel=parent 的子事务全部入列（含未预设类型按档三-4 通用显示）', async () => {
    const { sdk } = createMockSdk({ forgedDisposition: true });
    const service = new ProjectService(sdk);
    const children = await service.listChildren(PROJECT_ID);
    expect(children.map((c) => c.affairId)).toEqual([CHILD_BUG, CHILD_EPIC, CHILD_PR, CHILD_PROPOSAL]); // createdAt 升序
    const bug = children.find((c) => c.affairId === CHILD_BUG);
    expect(bug).toMatchObject({ type: 'bug', typeLabel: '缺陷', disposition: { state: 'open' }, resolutionBadge: 'open' });
    const epic = children.find((c) => c.affairId === CHILD_EPIC);
    expect(epic?.typeLabel).toContain('通用子事务');
    const pr = children.find((c) => c.affairId === CHILD_PR);
    expect(pr).toMatchObject({ type: 'pr', disposition: { state: 'open' }, resolutionBadge: 'open' });
    const proposal = children.find((c) => c.affairId === CHILD_PROPOSAL);
    // 伪造动议 + 伪造决议（集合外 actor，因果后继）都不采纳：维持 adopted（来自真决议 rr）
    expect(proposal?.disposition).toMatchObject({ state: 'adopted', opHash: 'rr', note: '好建议' });
    expect(proposal?.resolutionBadge).toBe('pending'); // 公示期内如实「待确认」
  });

  it('规则不可读 → 空写权集 fail-closed：处置全部回落开放（不伪造处置状态）', async () => {
    const { sdk } = createMockSdk({ rulesError: true });
    const service = new ProjectService(sdk);
    const children = await service.listChildren(PROJECT_ID);
    expect(children.find((c) => c.affairId === CHILD_PROPOSAL)?.disposition.state).toBe('open');
  });

  it('发起子事务：refs parent + maintainers 快照自 parent 现行规则 + 议题通告入项目日志', async () => {
    const { sdk, createdGenesisInputs, submittedOps } = createMockSdk();
    const service = new ProjectService(sdk);
    const { affairId, noticePosted } = await service.createChild(PROJECT_ID, { type: 'bug', title: ' 崩溃 ', summary: ' 复现 ' });
    const genesis = createdGenesisInputs[0];
    expect(genesis.type).toBe('bug');
    expect(genesis.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    expect((genesis.rules as Record<string, unknown>).maintainers).toEqual([ACTOR_IDENTITY]);
    expect(noticePosted).toBe(true);
    const notice = submittedOps.find((op) => (op.payload as Record<string, unknown>).kind === 'project.child-notice');
    expect(notice).toMatchObject({ affairId: PROJECT_ID });
    expect((notice?.payload as Record<string, unknown>).childAffairId).toBe(affairId);
  });

  it('未关注目标项目 / 创世未同步 → fail-closed 拒绝发起', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    await expect(service.createChild(OUTSIDER, { type: 'bug', title: 't', summary: 's' })).rejects.toThrow('尚未关注');
  });

  it('PR bundle cid 校验：非 64 位 hex 拒绝；合法 cid 入 extra.pr', async () => {
    const { sdk, createdGenesisInputs } = createMockSdk();
    const service = new ProjectService(sdk);
    await expect(
      service.createChild(PROJECT_ID, { type: 'pr', title: 't', summary: 's', bundleCid: 'not-hex' })
    ).rejects.toThrow('64 位小写 hex');
    await service.createChild(PROJECT_ID, { type: 'pr', title: 't', summary: 's', bundleCid: BUNDLE_CID });
    expect((createdGenesisInputs[0].extra as Record<string, unknown>).pr).toEqual({ bundleCid: BUNDLE_CID });
  });
});

describe('spark-project service · 维护者处置门控（档一-3 PR 回执 + R1 决议公示期）', () => {
  it('写权集合外身份 fail-closed 拒绝且不产出操作；空集合如实说明', async () => {
    const outsider = createMockSdk({ writeSet: [OUTSIDER] });
    const svc = new ProjectService(outsider.sdk);
    await expect(svc.submitDisposition(PROJECT_ID, CHILD_BUG, 'closed')).rejects.toThrow('维护者集合');
    expect(outsider.submittedOps).toHaveLength(0);
    const empty = createMockSdk({ writeSet: [] });
    await expect(new ProjectService(empty.sdk).submitDisposition(PROJECT_ID, CHILD_BUG, 'closed')).rejects.toThrow('未声明 maintainers');
  });

  it('规则不可读 → fail-closed 中止（不猜测写权）', async () => {
    const { sdk, submittedOps } = createMockSdk({ rulesError: true });
    const service = new ProjectService(sdk);
    await expect(service.submitDisposition(PROJECT_ID, CHILD_BUG, 'adopted')).rejects.toThrow('fail-closed');
    expect(submittedOps).toHaveLength(0);
  });

  it('归属校验（S1）：子事务创世 refs 不含对本项目的 parent 引用即拒，不动笔', async () => {
    const { sdk, submittedOps } = createMockSdk();
    const service = new ProjectService(sdk);
    // CHILD_EPIC 也是事务（自身规则可过写权校验），但 CHILD_BUG 不属于它
    await expect(service.submitDisposition(CHILD_EPIC, CHILD_BUG, 'closed')).rejects.toThrow('parent 引用');
    // 创世未同步的子事务同样 fail-closed
    await expect(service.submitDisposition(PROJECT_ID, '9'.repeat(64), 'closed')).rejects.toThrow('尚未同步');
    expect(submittedOps).toHaveLength(0);
  });

  it('bug/proposal：动议 + 决议两步入日志（R1）；决议载荷内核可复算', async () => {
    const { sdk, submittedOps } = createMockSdk();
    const service = new ProjectService(sdk);
    const result = await service.submitDisposition(PROJECT_ID, CHILD_PROPOSAL, 'closed', '重复反馈');
    expect(result.status).toBe('accepted');
    expect(result.resolutionOpHash).not.toBeNull();
    expect(submittedOps).toHaveLength(2);
    // 第一步：处置动议（content，prevOpHash = 子事务 DAG 头）
    const motion = submittedOps[0];
    expect(motion.affairId).toBe(CHILD_PROPOSAL);
    expect(motion.opType).toBe('content');
    expect(motion.prevOpHash).toBe('rr'); // fixture 的 DAG 头
    expect(motion.payload).toMatchObject({ kind: DISPOSITION_KIND, action: 'closed', note: '重复反馈' });
    expect(typeof motion.sig).toBe('string');
    // 第二步：决议操作（opType=resolution；condition 逐字回引规则，countedOps = 动议）
    const resolution = submittedOps[1];
    expect(resolution.opType).toBe('resolution');
    expect(resolution.prevOpHash).toBe(result.opHash); // 动议 opHash = 因果见证
    expect(resolution.payload).toMatchObject({
      result: 'closed',
      condition: { type: 'op-count', opType: 'content', filter: 'project.disposition', count: 1 },
      countedOps: [result.opHash],
      rulesHash: 'rh',
      pubPeriod: { delayMs: 86400000 }
    });
    expect((resolution.actor as Record<string, unknown>).identity).toBe(ACTOR_IDENTITY);
    expect(typeof resolution.sig).toBe('string');
  });

  it('处置 → 公示期 pending（徽标「待确认」）→ 公示期满 effective（内核状态经 readResolution 接通）', async () => {
    const { sdk, setResolutionState } = createMockSdk();
    const service = new ProjectService(sdk);
    await service.submitDisposition(PROJECT_ID, CHILD_BUG, 'adopted', '已修复');
    // 公示期内：处置状态已可读（决议入日志），徽标「待确认」而非「已生效」
    let bug = (await service.listChildren(PROJECT_ID)).find((c) => c.affairId === CHILD_BUG);
    expect(bug?.disposition).toMatchObject({ state: 'adopted', note: '已修复' });
    expect(bug?.resolutionBadge).toBe('pending');
    // 公示期满（mock 翻转内核推导状态）：徽标转「已生效」
    setResolutionState('effective');
    bug = (await service.listChildren(PROJECT_ID)).find((c) => c.affairId === CHILD_BUG);
    expect(bug?.resolutionBadge).toBe('effective');
  });

  it('PR：单维护者合并回执即生效（档一-3），不投决议操作', async () => {
    const { sdk, submittedOps } = createMockSdk();
    const service = new ProjectService(sdk);
    const result = await service.submitDisposition(PROJECT_ID, CHILD_PR, 'adopted', '已合并，新镜像 cid 见附言');
    expect(result.status).toBe('accepted');
    expect(result.resolutionOpHash).toBeNull();
    expect(submittedOps).toHaveLength(1); // 仅回执 content op
    expect(submittedOps[0].opType).toBe('content');
    // receipt 形态读侧：回执即生效，徽标「已生效」（无公示期，不显示「待确认」）
    const pr = (await service.listChildren(PROJECT_ID)).find((c) => c.affairId === CHILD_PR);
    expect(pr?.disposition.state).toBe('adopted');
    expect(pr?.resolutionBadge).toBe('effective');
  });
});

describe('spark-project service · 文档与草稿（§3.2 集合纪律）', () => {
  it('文档 append-only 版本链：seq 递增、键不覆盖、历史可溯', async () => {
    const { sdk, dataDocs } = createMockSdk();
    const service = new ProjectService(sdk);
    const v1 = await service.saveDoc(PROJECT_ID, { title: '设计稿', body: 'v1' });
    const v2 = await service.saveDoc(PROJECT_ID, { docId: v1.docId, title: '设计稿', body: 'v2' });
    expect(v1.seq).toBe(1);
    expect(v2.seq).toBe(2);
    expect(dataDocs.has(`spark-project:docs/${PROJECT_ID}/${v1.docId}/1`)).toBe(true);
    expect(dataDocs.has(`spark-project:docs/${PROJECT_ID}/${v1.docId}/2`)).toBe(true);
    const summaries = await service.listDocs(PROJECT_ID);
    expect(summaries).toHaveLength(1);
    expect(summaries[0]).toMatchObject({ docId: v1.docId, latestSeq: 2, versionCount: 2 });
    expect((await service.getDocHistory(PROJECT_ID, v1.docId)).map((v) => v.seq)).toEqual([1, 2]);
    // mock enforce 旁证：同键覆盖被 append-only 拒绝（版本链键设计天然不覆盖）
    await expect(sdk.data.save('spark-project:docs', `${PROJECT_ID}/${v1.docId}/1`, v2)).rejects.toThrow('AppendOnlyViolation');
  });

  it('seq 冲突捕获重试一次（S3）：并发竞对抢占同键后重读历史重算 seq', async () => {
    const { sdk, dataDocs } = createMockSdk({ raceDocSave: true });
    const service = new ProjectService(sdk);
    const version = await service.saveDoc(PROJECT_ID, { docId: 'doc-fixed', title: '设计稿', body: '我的版本' });
    // 首次保存被竞对抢占 seq 1 键（mock 注入），重试后 seq=2 成功
    expect(version.seq).toBe(2);
    expect(dataDocs.get(`spark-project:docs/${PROJECT_ID}/doc-fixed/1`)).toMatchObject({ seq: 1, body: 'concurrent-write' });
    expect(dataDocs.get(`spark-project:docs/${PROJECT_ID}/doc-fixed/2`)).toMatchObject({ seq: 2, body: '我的版本' });
    expect((await service.getDocHistory(PROJECT_ID, 'doc-fixed')).map((v) => v.seq)).toEqual([1, 2]);
  });

  it('草稿 scope=local：保存/按项目过滤/删除', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    const id = await service.saveDraft({ projectAffairId: PROJECT_ID, type: 'bug', title: '草稿', summary: 's' });
    await service.saveDraft({ projectAffairId: CHILD_BUG, type: 'bug', title: '别的项目', summary: 's' });
    const drafts = await service.listDrafts(PROJECT_ID);
    expect(drafts).toHaveLength(1);
    expect(drafts[0]).toMatchObject({ id, title: '草稿' });
    await service.deleteDraft(id);
    expect(await service.listDrafts(PROJECT_ID)).toHaveLength(0);
  });
});

describe('spark-project service · 成员页（内核推导原样呈现 + 维护者标注）', () => {
  it('阶梯名册原样 + maintainers 交集标注', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    const { members } = await service.listMembers(PROJECT_ID);
    const me = members.find((m) => m.identity === ACTOR_IDENTITY);
    expect(me).toMatchObject({ tier: 'contributor', accepts: 2, isMaintainer: true });
    expect(members.find((m) => m.identity === OUTSIDER)?.isMaintainer).toBe(false);
    const profile = await service.getPublicProfile(ACTOR_IDENTITY);
    expect(profile.affairsParticipated).toBe(1);
  });
});

describe('spark-project service · 看板组合（库依赖 vendor spark-kanban；只读 MVP）', () => {
  it('未初始化 → null；初始化后子事务自动聚合成卡片（档三-1 权威源）', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    expect(await service.projectBoardView(PROJECT_ID, 'org-1')).toBeNull();
    const init = await service.initProjectBoard(PROJECT_ID, 'org-1', 'root-admin', 'admin', '星火项目');
    expect(init.ok).toBe(true);
    const view = await service.projectBoardView(PROJECT_ID, 'org-1');
    expect(view).not.toBeNull();
    expect(view!.columns.length).toBe(5); // 库件默认列模板：待分诊/待办/进行中/待验证/完成
    const cardIds = view!.columns.flatMap((column) =>
      column.cards.filter((card) => card.kind === 'affair').map((card) => (card as { kind: 'affair'; card: { affairId: string } }).card.affairId)
    );
    expect(cardIds.sort()).toEqual([CHILD_BUG, CHILD_EPIC, CHILD_PR, CHILD_PROPOSAL].sort());
    // 工作区配置写本插件命名空间（§4「看板配置写本插件命名空间」）
    expect(sdk.data.save.mock.calls.some((call: any[]) => call[0] === 'spark-project:workspace')).toBe(true);
  });

  it('组织空间非管理员建板被拒：如实上报原因，不静默不绕过', async () => {
    const { sdk } = createMockSdk();
    const service = new ProjectService(sdk);
    const result = await service.initProjectBoard(PROJECT_ID, 'org-1', 'root-member', 'member', '星火项目');
    expect(result.ok).toBe(false);
    expect((result as { ok: false; reason: string }).reason).toContain('管理员');
    // 个人空间不受组织治理约束
    const personal = await service.initProjectBoard(PROJECT_ID, 'personal', 'root-member', null, '个人项目');
    expect(personal.ok).toBe(true);
  });
});

describe('spark-project service · 发布组合（库依赖 vendor spark-release-manager；档一-2）', () => {
  const setupReleases = async () => {
    const mock = createMockSdk();
    const service = new ProjectService(mock.sdk);
    await service.initReleaseConfig('org-1', ADMIN, 'admin');
    const release = await service.registerRelease('org-1', ADMIN, {
      pluginId: 'spark-foo',
      version: '0.1.0',
      updateManifestJson: UPDATE_MANIFEST
    });
    return { ...mock, service, release };
  };

  it('登记 → 本机核验 → 推进发布 全链（核验未过不得推进是状态机硬约束）', async () => {
    const { service, release, sentMessages } = await setupReleases();
    // 状态机硬约束：registered 直接推进被拒
    await expect(service.publishRelease('org-1', ADMIN, release.id)).rejects.toThrow('核验未通过');
    const verified = await service.verifyRelease('org-1', ADMIN, release.id, '/tmp/x.spkg');
    expect(verified.type).toBe('verified');
    const published = await service.publishRelease('org-1', ADMIN, release.id);
    expect(published.type).toBe('published');
    // 版本卡片唯一推送源 = 发布管理件（本插件不自推）：viewId=release-card + 引用载荷
    const card = sentMessages.find((m) => (m.card as { viewId?: string })?.viewId === 'release-card');
    expect(card?.card).toMatchObject({ viewId: 'release-card', data: { releaseId: release.id, orgId: 'org-1' } });
    expect(String(card?.payload.releaseRef)).toBe(release.id); // 幂等键（档一-2/档三-24）
    const { rows } = await service.listReleaseRows('org-1', [ADMIN]);
    expect(rows[0]).toMatchObject({ state: 'published', stateLabel: '已发布' });
    const detail = await service.getReleaseDetail('org-1', release.id, [ADMIN]);
    expect(detail?.state).toBe('published');
    const evidence = await service.getReleaseEvidence();
    expect(evidence.headHash).toMatch(/^[0-9a-f]{64}$/);
  });

  it('登记幂等键：（组织, 插件, 版本）唯一，重复登记被拒', async () => {
    const { service } = await setupReleases();
    await expect(
      service.registerRelease('org-1', ADMIN, { pluginId: 'spark-foo', version: '0.1.0', updateManifestJson: UPDATE_MANIFEST })
    ).rejects.toThrow('幂等键');
  });

  it('读侧鉴权：伪造操作者事件不参与状态派生（直写 docs 旁路服务层守卫）', async () => {
    const { sdk, service, release, docsStore } = await setupReleases();
    void docsStore;
    const forged: ReleaseEvent = {
      id: 'evt-forged',
      orgId: 'org-1',
      releaseId: release.id,
      type: 'published',
      operatorRootId: OUTSIDER,
      at: 1
    };
    await sdk.docs.put(RELEASE_COLLECTIONS.events, forged.id, forged as unknown as Record<string, unknown>);
    const { rows } = await service.listReleaseRows('org-1', [ADMIN]);
    expect(rows[0].state).toBe('registered'); // 伪造的 published 不生效
  });

  it('成员侧补发：台账按 releaseRef 幂等去重；配置不可得 fail-closed 不补发', async () => {
    const { sdk, service, release, sentMessages } = await setupReleases();
    expect(await service.backfillReleaseCards('org-unknown', [])).toBe(0); // 无配置 → fail-closed
    await service.verifyRelease('org-1', ADMIN, release.id, '/tmp/x.spkg');
    await service.publishRelease('org-1', ADMIN, release.id); // 发布者即时推送并入台账
    const before = sentMessages.length;
    expect(await service.backfillReleaseCards('org-1', [ADMIN])).toBe(0); // 已送达不重复
    expect(sentMessages.length).toBe(before);
    // 直注一条未送达的合法 published 事件（如同步到达的它机发布）→ 补发 1 张
    const synced: ReleaseEvent = { id: 'evt-synced', orgId: 'org-1', releaseId: release.id, type: 'published', operatorRootId: ADMIN, at: 2 };
    // 台账不含第二个发布单：另登记一单并由它机事件标记 published
    const second = await service.registerRelease('org-1', ADMIN, { pluginId: 'spark-bar', version: '2.0.0', updateManifestJson: UPDATE_MANIFEST });
    await sdk.docs.put(RELEASE_COLLECTIONS.events, synced.id, synced as unknown as Record<string, unknown>);
    await sdk.docs.put(RELEASE_COLLECTIONS.events, 'evt-synced-2', {
      id: 'evt-synced-2', orgId: 'org-1', releaseId: second.id, type: 'published', operatorRootId: ADMIN, at: 3
    } as unknown as Record<string, unknown>);
    const sent = await service.backfillReleaseCards('org-1', [ADMIN]);
    expect(sent).toBe(1);
    expect(sentMessages[sentMessages.length - 1].card).toMatchObject({ viewId: 'release-card', data: { releaseId: second.id } });
    expect(await service.backfillReleaseCards('org-1', [ADMIN])).toBe(0); // 再次补发幂等
  });

  it('无市场模块（移动端/未授权）→ 核验如实报错委托桌面端（档二-8）', async () => {
    const mock = createMockSdk({ withMarket: false });
    const service = new ProjectService(mock.sdk);
    expect(service.releaseMarketAvailable).toBe(false);
    await service.initReleaseConfig('org-1', ADMIN, 'admin');
    const release = await service.registerRelease('org-1', ADMIN, {
      pluginId: 'spark-foo',
      version: '0.1.0',
      updateManifestJson: UPDATE_MANIFEST
    });
    await expect(service.verifyRelease('org-1', ADMIN, release.id, '/tmp/x.spkg')).rejects.toThrow('委托桌面端');
  });
});

describe('spark-project service · 通知降级（档二-4：加载时补发 + 节流 + 台账去重）', () => {
  it('处置卡片补发：台账去重 + 限流中止不记账', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const service = new ProjectService(sdk);
    // CHILD_PROPOSAL 已有有效 adopted 处置 → 补发 1 张
    expect(await service.backfillDispositionCards()).toBe(1);
    expect(sentMessages).toHaveLength(1);
    expect(sentMessages[0].card).toMatchObject({ viewId: 'affair-card', data: { kind: 'child-disposition', affairId: CHILD_PROPOSAL, action: 'adopted' } });
    expect(String(sentMessages[0].payload.summary)).toContain('深色模式');
    // 台账去重：再次补发为 0
    expect(await service.backfillDispositionCards()).toBe(0);
  });

  it('限流/权限拒绝即中止本轮且未送达不记账（下次加载补齐）', async () => {
    const { sdk, sentMessages } = createMockSdk({ messageError: true });
    const service = new ProjectService(sdk);
    expect(await service.backfillDispositionCards()).toBe(0);
    expect(sentMessages).toHaveLength(0);
    const ledger = await sdk.data.query('spark-project:notified', { prefix: 'disp:' });
    expect(ledger.items).toHaveLength(0); // 未送达不记账
  });

  it('messages 模块缺席 → 静默降级返回 0', async () => {
    const { sdk } = createMockSdk();
    sdk.messages = undefined;
    const service = new ProjectService(sdk);
    expect(await service.backfillDispositionCards()).toBe(0);
  });
});

describe('spark-project service · 事务模块缺失降级', () => {
  it('无 sdk.affairs：事务面整体 fail-closed 提示，数据面不受影响', async () => {
    const { sdk } = createMockSdk({ withAffairs: false });
    const service = new ProjectService(sdk);
    expect(service.affairsAvailable).toBe(false);
    await expect(service.listProjects()).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    await expect(service.createProject({ title: 't', summary: 's', tags: [], publish: false })).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    await expect(service.submitComment(PROJECT_ID, 'hi')).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    // 文档面（sdk.data）独立可用
    const doc = await service.saveDoc(PROJECT_ID, { title: '离线文档', body: 'x' });
    expect(doc.seq).toBe(1);
  });
});
