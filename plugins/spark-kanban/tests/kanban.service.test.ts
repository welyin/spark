import { beforeEach, describe, expect, it, vi } from 'vitest';
import { AFFAIRS_MODULE_MISSING, KANBAN_COLLECTIONS, KanbanService, kanbanCollections } from '../service';
import { defaultBoardColumns, type KanbanBoard, type KanbanCardOp } from '../model';
import { deriveIdentity } from '../wire';

/**
 * mock SDK：data 为内存集合后端（declareCollection/save/get/query/delete，按声明
 * enforce append-only——对齐内核 plugindata 口径，防 mock 过松掩盖违规调用）；
 * affairs/identity/messages 按已落地 SDK 面实现（钉住真实契约）。
 */
const PROJECT_ID = 'ab'.repeat(32);
const CHILD_BUG = 'cd'.repeat(32);
const CHILD_PROPOSAL = 'ef'.repeat(32);
// 插件域身份公钥（base64 合法值；actor identity = deriveIdentity(publicKey)）
const PLUGIN_PK = 'cGsx';
const ACTOR_IDENTITY = deriveIdentity(PLUGIN_PK);

type MockOptions = {
  withAffairs?: boolean;
  signError?: boolean;
  writeSet?: string[];
  submitStatus?: 'accepted' | 'pending' | 'duplicate';
  /** 注入一条写权集合外 actor 的伪造状态操作（未来 declaredAt + 挂在真实头之后） */
  forgedStatusOp?: boolean;
};

function createMockSdk(options: MockOptions = {}) {
  const dataDocs = new Map<string, unknown>();
  const collectionMerge = new Map<string, string>();
  const submittedOps: Array<Record<string, unknown>> = [];
  const sentMessages: Array<{ payload: Record<string, unknown>; card?: unknown }> = [];

  const genesisOf = (type: string, title: string, parentTarget?: string): Record<string, unknown> => ({
    affairV: 1,
    type,
    title,
    summary: `${title} 摘要`,
    tags: [],
    initiator: { kind: 'person', identity: 'x'.repeat(64), publicKey: PLUGIN_PK },
    refs: parentTarget ? [{ target: parentTarget, rel: 'parent' }] : [],
    createdAt: 100,
    rules: {}
  });

  const affairs = options.withAffairs === false
    ? undefined
    : {
        listFollowed: vi.fn().mockResolvedValue([CHILD_BUG, CHILD_PROPOSAL, PROJECT_ID].sort()),
        readLog: vi.fn().mockImplementation(async (affairId: string) => {
          if (affairId === CHILD_BUG) {
            return { affairId, genesis: genesisOf('bug', '启动崩溃', PROJECT_ID), ops: [], heads: [CHILD_BUG], followedAt: 1 };
          }
          if (affairId === CHILD_PROPOSAL) {
            const ops: Array<{ opHash: string; op: Record<string, unknown> }> = [
              {
                opHash: 'aa',
                op: {
                  opV: 1,
                  affairId: CHILD_PROPOSAL,
                  opType: 'content',
                  prevOpHash: CHILD_PROPOSAL,
                  payload: { kind: 'kanban.status', status: 'doing' },
                  actor: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PLUGIN_PK },
                  declaredAt: 50
                }
              }
            ];
            if (options.forgedStatusOp) {
              // 伪造：集合外 actor + 未来远日 declaredAt + 因果上挂在真实头之后
              ops.push({
                opHash: 'zz',
                op: {
                  opV: 1,
                  affairId: CHILD_PROPOSAL,
                  opType: 'content',
                  prevOpHash: 'aa',
                  payload: { kind: 'kanban.status', status: 'verifying' },
                  actor: { kind: 'person', identity: 'b'.repeat(64), publicKey: PLUGIN_PK },
                  declaredAt: Number.MAX_SAFE_INTEGER
                }
              });
            }
            return { affairId, genesis: genesisOf('proposal', '深色模式', PROJECT_ID), ops, heads: ['aa'], followedAt: 1 };
          }
          // 项目议题自身：创世不含 parent 自指
          return { affairId, genesis: genesisOf('project', '星火项目'), ops: [], heads: [affairId], followedAt: 1 };
        }),
        readResolution: vi.fn().mockImplementation(async (affairId: string) => ({
          affairId,
          resolutions:
            affairId === CHILD_PROPOSAL
              ? [{ opHash: 'rr', state: 'pending' as const, result: null, condition: null, countedOps: null, rulesHash: 'h', pubPeriodMs: 86400000, anchoredMs: 1, objections: 0 }]
              : []
        })),
        readRules: vi.fn().mockImplementation(async (affairId: string) => ({
          affairId,
          nowMs: 1000,
          current: { seq: 0, rulesHash: 'rh', rules: { maintainers: options.writeSet ?? [ACTOR_IDENTITY] } },
          versions: [],
          changes: []
        })),
        submitOp: vi.fn().mockImplementation(async (op: Record<string, unknown>) => {
          submittedOps.push(op);
          return { affairId: op.affairId as string, opHash: 'op-hash-1', status: options.submitStatus ?? 'accepted' };
        }),
        onChange: vi.fn().mockResolvedValue(undefined)
      };

  const sdk = {
    domain: 'plugin:spark-kanban',
    affairs,
    identity: {
      sign: vi.fn().mockImplementation(async (payload: string) => {
        if (options.signError) {
          throw new Error('用户拒绝签名授权');
        }
        return { domain: 'plugin:spark-kanban', domainId: 'spark-kanban', publicKey: PLUGIN_PK, signature: 'sig-1', payloadHash: 'h' };
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    data: {
      declareCollection: vi.fn().mockImplementation(async (decl: { name: string; merge?: string }) => {
        collectionMerge.set(decl.name, decl.merge ?? 'lww-record');
        return {};
      }),
      get: vi.fn().mockImplementation(async (name: string, key: string) => dataDocs.get(`${name}/${key}`) ?? null),
      save: vi.fn().mockImplementation(async (name: string, key: string, value: unknown) => {
        if (collectionMerge.get(name) === 'append-only' && dataDocs.has(`${name}/${key}`)) {
          throw new Error(`AppendOnlyViolation: save-overwrite rejected on ${name}（mock enforce）`);
        }
        dataDocs.set(`${name}/${key}`, value);
        return { success: true };
      }),
      delete: vi.fn().mockImplementation(async (name: string, key: string) => {
        if (collectionMerge.get(name) === 'append-only' && dataDocs.has(`${name}/${key}`)) {
          throw new Error(`AppendOnlyViolation: delete rejected on ${name}（mock enforce）`);
        }
        dataDocs.delete(`${name}/${key}`);
        return { success: true };
      }),
      query: vi.fn().mockImplementation(async (name: string, opts?: { prefix?: string }) => ({
        items: [...dataDocs.entries()]
          .filter(([k]) => k.startsWith(`${name}/${opts?.prefix ?? ''}`))
          .map(([k, value]) => ({ key: k.slice(name.length + 1), value }))
      })),
      onChange: vi.fn().mockResolvedValue(undefined)
    },
    messages: {
      sendAppMessage: vi.fn().mockImplementation(async (payload: Record<string, unknown>, card?: unknown) => {
        sentMessages.push({ payload, card });
        return { id: `m-${sentMessages.length}` };
      }),
      onCardAction: vi.fn()
    }
  } as any;

  return { sdk, dataDocs, collectionMerge, submittedOps, sentMessages };
}

const mkBoard = (overrides: Partial<KanbanBoard> = {}): KanbanBoard => ({
  id: 'board-1',
  orgId: 'org-1',
  name: '迭代看板',
  contextAffairId: PROJECT_ID,
  columns: defaultBoardColumns(),
  createdBy: 'root-admin',
  createdAt: 1,
  updatedAt: 1,
  ...overrides
});

describe('spark-kanban service · 集合与看板配置', () => {
  beforeEach(() => {
    globalThis.localStorage?.clear();
  });

  it('declares all four collections with designed merge/scope before writing (§3.5)', async () => {
    const { sdk, collectionMerge } = createMockSdk();
    const service = new KanbanService(sdk);
    await service.createBoard('org-1', 'root-admin', { name: '迭代看板' }, 'admin');

    expect([...collectionMerge.entries()].sort()).toEqual([
      [KANBAN_COLLECTIONS.bindings, 'append-only'],
      [KANBAN_COLLECTIONS.boards, 'lww-record'],
      [KANBAN_COLLECTIONS.cardOps, 'append-only'],
      [KANBAN_COLLECTIONS.viewPrefs, 'lww-record']
    ]);
    const scopes = sdk.data.declareCollection.mock.calls.map((call: any[]) => [call[0].name, call[0].scope]);
    expect(scopes).toEqual([
      [KANBAN_COLLECTIONS.boards, 'sync'],
      [KANBAN_COLLECTIONS.cardOps, 'sync'],
      [KANBAN_COLLECTIONS.bindings, 'sync'],
      // 档三-22：列内手动排序 = local 视图偏好，不进同步流量
      [KANBAN_COLLECTIONS.viewPrefs, 'local']
    ]);
    // 声明幂等：第二次写入不重复声明
    await service.createBoard('org-1', 'root-admin', { name: '第二看板' }, 'admin');
    expect(sdk.data.declareCollection).toHaveBeenCalledTimes(4);
  });

  it('declares collections under the composer namespace when embedded as a library（硬伤 3 回归）', async () => {
    // 库包形态：被 spark-project 构建期组合后，集合名前缀必须是组合者插件 id
    // （内核 plugindata NamePrefixMismatch 防线），数据落组合者命名空间
    expect(kanbanCollections('spark-project')).toEqual({
      boards: 'spark-project:boards',
      cardOps: 'spark-project:card-ops',
      bindings: 'spark-project:bindings',
      viewPrefs: 'spark-project:view-prefs',
      notifyLedger: 'spark-project:notify-ledger'
    });
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk, 'spark-project');
    await service.createBoard('org-1', 'root-admin', { name: '组合看板' }, 'admin');
    const names = sdk.data.declareCollection.mock.calls.map((call: any[]) => call[0].name);
    expect(names).toEqual([
      'spark-project:boards',
      'spark-project:card-ops',
      'spark-project:bindings',
      'spark-project:view-prefs'
    ]);
    expect(sdk.data.save.mock.calls[0][0]).toBe('spark-project:boards');
    // 缺省命名空间 = 独立安装形态
    expect(kanbanCollections('spark-kanban')).toEqual(KANBAN_COLLECTIONS);
  });

  it('creates board with default column template and optional context affairId', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    const board = await service.createBoard('org-1', 'root-admin', { name: ' 迭代看板 ', contextAffairId: PROJECT_ID }, 'admin');
    expect(board.name).toBe('迭代看板');
    expect(board.columns.map((c) => c.kind)).toEqual(['triage', 'stage', 'stage', 'stage', 'terminal']);
    expect(board.contextAffairId).toBe(PROJECT_ID);
    const loaded = await service.getBoard('org-1', board.id);
    expect(loaded?.name).toBe('迭代看板');
  });

  it('rejects board creation by non-admin in org space; personal space is exempt', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    await expect(service.createBoard('org-1', 'root-member', { name: 'x' }, 'member')).rejects.toThrow('管理员');
    const personal = await service.createBoard('personal', 'root-member', { name: '个人看板' }, null);
    expect(personal.orgId).toBe('personal');
  });
});

describe('spark-kanban service · 原生卡片操作流（append-only + 签名）', () => {
  it('creates card into triage column with signature; members allowed', async () => {
    const { sdk, dataDocs } = createMockSdk();
    const service = new KanbanService(sdk);
    const board = mkBoard();
    const op = await service.createCard('org-1', 'root-member', board, { title: '修崩溃', description: '复现步骤…' }, 'member');
    expect(op.kind).toBe('create');
    expect(op.columnId).toBe('triage');
    expect(op.signature?.signature).toBe('sig-1');
    // 写入卡片操作集合（append-only；键带 orgId 前缀）
    expect(dataDocs.has(`${KANBAN_COLLECTIONS.cardOps}/org-1/${op.id}`)).toBe(true);
  });

  it('degrades gracefully when signing is refused (record saved without signature badge)', async () => {
    const { sdk } = createMockSdk({ signError: true });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const service = new KanbanService(sdk);
    const op = await service.createCard('org-1', 'root-member', mkBoard(), { title: '无签名卡' }, 'member');
    expect(op.signature).toBeUndefined();
    warn.mockRestore();
  });

  it('rejects card ops from non-members in org space', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    await expect(service.createCard('org-1', 'root-out', mkBoard(), { title: 'x' }, null)).rejects.toThrow('组织成员');
    await expect(service.moveCard('org-1', 'root-out', 'board-1', 'card-1', 'doing', null)).rejects.toThrow('组织成员');
  });

  it('move/assign/comment append distinct ops (never overwrite — mock enforces append-only)', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    const board = mkBoard();
    const create = await service.createCard('org-1', 'root-1', board, { title: 't' }, 'admin');
    await service.moveCard('org-1', 'root-1', board.id, create.cardId, 'doing', 'admin');
    await service.assignCard('org-1', 'root-1', board.id, create.cardId, 'root-2', 'admin');
    await service.commentCard('org-1', 'root-1', board.id, create.cardId, '在看', 'admin');
    const ops = await service.loadCardOps('org-1');
    expect(ops.map((op) => op.kind).sort()).toEqual(['assign', 'comment', 'create', 'move']);
    expect(sdk.data.save).toHaveBeenCalledTimes(4);
  });
});

describe('spark-kanban service · 绑定记录（档三-20）', () => {
  it('enforces native card → single affair (re-bind requires unbind first)', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    const first = await service.bindNativeCard('org-1', 'root-1', 'board-1', 'card-1', CHILD_BUG, [], 'admin');
    const existing = await service.loadBindings('org-1');
    await expect(
      service.bindNativeCard('org-1', 'root-1', 'board-1', 'card-1', CHILD_PROPOSAL, existing, 'admin')
    ).rejects.toThrow('单一子事务');
    await service.unbind('org-1', 'root-1', first, 'admin');
    const after = await service.loadBindings('org-1');
    const rebound = await service.bindNativeCard('org-1', 'root-1', 'board-1', 'card-1', CHILD_PROPOSAL, after, 'admin');
    expect(rebound.affairId).toBe(CHILD_PROPOSAL);
    // append-only：bind/unbind/再绑 三条记录全部留痕
    expect((await service.loadBindings('org-1')).map((b) => b.kind)).toEqual(['bind', 'unbind', 'bind']);
  });

  it('allows the same sub-affair on multiple boards (绑定记录即视图数据)', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    await service.bindAffair('org-1', 'root-1', 'board-1', CHILD_BUG, 'admin');
    await service.bindAffair('org-1', 'root-1', 'board-2', CHILD_BUG, 'admin');
    expect((await service.loadBindings('org-1'))).toHaveLength(2);
  });

  it('rejects malformed affairId on bind paths', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    await expect(service.bindAffair('org-1', 'root-1', 'board-1', 'not-hex', 'admin')).rejects.toThrow('64 位小写 hex');
    await expect(service.bindNativeCard('org-1', 'root-1', 'board-1', 'c', 'ZZZ', [], 'admin')).rejects.toThrow('hex');
  });
});

describe('spark-kanban service · 视图偏好（local）', () => {
  it('saves per-column manual order merged into existing prefs', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    await service.saveCardOrder('board-1', 'todo', ['native:a', 'native:b']);
    const prefs = await service.saveCardOrder('board-1', 'doing', ['affair:x']);
    expect(prefs.cardOrder).toEqual({ todo: ['native:a', 'native:b'], doing: ['affair:x'] });
    const loaded = await service.loadViewPrefs('board-1');
    expect(loaded?.cardOrder['todo']).toEqual(['native:a', 'native:b']);
  });
});

describe('spark-kanban service · 子事务聚合与状态操作（档三-1/档三-19）', () => {
  it('aggregates child affairs of the board context into cards (自动绑定：refs parent 匹配)', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    const cards = await service.listAffairCards(mkBoard(), []);
    // 项目议题自身不是卡片；两个子事务自动成为卡片
    expect(cards.map((c) => c.affairId).sort()).toEqual([CHILD_BUG, CHILD_PROPOSAL].sort());
    const bug = cards.find((c) => c.affairId === CHILD_BUG);
    expect(bug).toMatchObject({ affairType: 'bug', statusKey: null, resolution: 'open' });
    const proposal = cards.find((c) => c.affairId === CHILD_PROPOSAL);
    // 状态操作 → statusKey；决议公示中 → pending（待确认）
    expect(proposal).toMatchObject({ statusKey: 'doing', resolution: 'pending' });
  });

  it('read-side write-set filtering: forged status op (outsider actor, future declaredAt, causal descendant) does not change derived state（硬伤 1+2 回归）', async () => {
    const { sdk } = createMockSdk({ forgedStatusOp: true });
    const service = new KanbanService(sdk);
    const cards = await service.listAffairCards(mkBoard(), []);
    const proposal = cards.find((c) => c.affairId === CHILD_PROPOSAL);
    // 伪造操作因果上是真实头的后继且 declaredAt 远未来——但 actor ∉ 写权集合，不采纳
    expect(proposal?.statusKey).toBe('doing');
  });

  it('read-side fails closed when rules are unreadable (空写权集 = 状态操作全部不算，卡片退回未分诊)', async () => {
    const { sdk } = createMockSdk();
    sdk.affairs.readRules.mockRejectedValue(new Error('规则文档尚未同步'));
    const service = new KanbanService(sdk);
    const cards = await service.listAffairCards(mkBoard(), []);
    expect(cards.find((c) => c.affairId === CHILD_PROPOSAL)?.statusKey).toBeNull();
  });

  it('includes manually bound affairs outside the context (补充路径)', async () => {
    const { sdk } = createMockSdk();
    const service = new KanbanService(sdk);
    // 无 contextAffairId 的看板 + 手动挂载 CHILD_BUG
    const board = mkBoard({ contextAffairId: undefined });
    const binding = await service.bindAffair('org-1', 'root-1', board.id, CHILD_BUG, 'admin');
    const cards = await service.listAffairCards(board, [binding]);
    expect(cards.map((c) => c.affairId)).toEqual([CHILD_BUG]);
  });

  it('submits signed status op into affair log (prevOpHash = local DAG head, prevStatus derived)', async () => {
    const { sdk, submittedOps } = createMockSdk();
    const service = new KanbanService(sdk);
    const result = await service.submitStatusOp(CHILD_PROPOSAL, 'verifying', '修完了待验证');
    expect(result.status).toBe('accepted');
    expect(submittedOps).toHaveLength(1);
    const op = submittedOps[0];
    expect(op.opType).toBe('content');
    expect(op.affairId).toBe(CHILD_PROPOSAL);
    expect(op.prevOpHash).toBe('aa'); // 本地 DAG 头
    expect((op.payload as any)).toMatchObject({ kind: 'kanban.status', status: 'verifying', prevStatus: 'doing', note: '修完了待验证' });
    expect(typeof op.sig).toBe('string'); // 签名是内核入站硬要求
    expect((op.actor as any).identity).toBe(ACTOR_IDENTITY);
  });

  it('fails closed when actor is outside the write set (档三-19：写权集合成员)', async () => {
    const { sdk, submittedOps } = createMockSdk({ writeSet: ['f'.repeat(64)] });
    const service = new KanbanService(sdk);
    await expect(service.submitStatusOp(CHILD_BUG, 'todo')).rejects.toThrow('写权集合');
    expect(submittedOps).toHaveLength(0); // 不产出伪造状态操作
  });

  it('fails closed with empty maintainers (无人可提交，如实说明)', async () => {
    const { sdk } = createMockSdk({ writeSet: [] });
    const service = new KanbanService(sdk);
    await expect(service.submitStatusOp(CHILD_BUG, 'todo')).rejects.toThrow('未声明 maintainers');
  });

  it('surfaces non-accepted submit verdicts honestly (pending = 未知指向暂存)', async () => {
    const { sdk } = createMockSdk({ submitStatus: 'pending' });
    const service = new KanbanService(sdk);
    const result = await service.submitStatusOp(CHILD_PROPOSAL, 'verifying');
    expect(result.status).toBe('pending');
  });

  it('degrades without affairs module (独立使用：绑定入口隐藏，不报错)', async () => {
    const { sdk } = createMockSdk({ withAffairs: false });
    const service = new KanbanService(sdk);
    expect(service.affairsAvailable).toBe(false);
    await expect(service.listAffairCards(mkBoard(), [])).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    await expect(service.submitStatusOp(CHILD_BUG, 'todo')).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    // 原生卡片路径不受影响
    const op = await service.createCard('org-1', 'root-1', mkBoard(), { title: 'x' }, 'admin');
    expect(op.kind).toBe('create');
  });
});

describe('spark-kanban service · 指派通知（档三-12 最少事件集）', () => {
  it('notifies only assign ops targeting current identity, deduped via local ledger', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const service = new KanbanService(sdk);
    const board = mkBoard();
    const create = await service.createCard('org-1', 'root-1', board, { title: '修崩溃' }, 'admin');
    const assignMe = await service.assignCard('org-1', 'root-1', board.id, create.cardId, 'root-me', 'admin');
    await service.assignCard('org-1', 'root-1', board.id, create.cardId, 'root-other', 'admin');
    const ops = await service.loadCardOps('org-1');

    const sent = await service.notifyAssignedToMe('org-1', 'root-me', ops, board.name);
    // 最新一条 assign 指向 root-other：以操作流为准只有指向我的操作通知
    expect(sent).toBe(1);
    expect(sentMessages).toHaveLength(1);
    expect(String(sentMessages[0].payload.summary)).toContain('修崩溃');
    expect(sentMessages[0].card).toMatchObject({ viewId: 'card-notify', data: { cardId: create.cardId, boardId: board.id, orgId: 'org-1' } });

    // 去重台账：同一批操作不重复通知
    const again = await service.notifyAssignedToMe('org-1', 'root-me', ops, board.name);
    expect(again).toBe(0);
    expect(assignMe.assigneeRootId).toBe('root-me');
    // 台账持久面：懒声明 scope:'local' append-only 集合并按 {orgId}:{opId} 写入
    const ledgerDecl = sdk.data.declareCollection.mock.calls.find((call: any[]) => call[0].name === KANBAN_COLLECTIONS.notifyLedger);
    expect(ledgerDecl?.[0]).toMatchObject({ merge: 'append-only', scope: 'local' });
    expect(sdk.data.save.mock.calls.some((call: any[]) => call[0] === KANBAN_COLLECTIONS.notifyLedger && String(call[1]).startsWith('org-1:'))).toBe(true);
  });

  it('dedups via persistent ledger across instances when localStorage is unavailable（A61：opaque origin 沙箱）', async () => {
    // 模拟 opaque origin iframe：访问 localStorage 恒抛 SecurityError
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      get() {
        throw new Error('SecurityError: opaque origin');
      },
      configurable: true
    });
    try {
      const { sdk } = createMockSdk();
      const board = mkBoard();
      // 实例 A：建卡 + 指派 + 通知（台账写 sdk.data 持久面）
      const serviceA = new KanbanService(sdk);
      const create = await serviceA.createCard('org-1', 'root-1', board, { title: '修崩溃' }, 'admin');
      await serviceA.assignCard('org-1', 'root-1', board.id, create.cardId, 'root-me', 'admin');
      const ops = await serviceA.loadCardOps('org-1');
      expect(await serviceA.notifyAssignedToMe('org-1', 'root-me', ops, board.name)).toBe(1);

      // 实例 B（模拟插件重开：进程内兜底已清零）：持久台账去重，不重发
      const serviceB = new KanbanService(sdk);
      expect(await serviceB.notifyAssignedToMe('org-1', 'root-me', ops, board.name)).toBe(0);
    } finally {
      if (descriptor) {
        Object.defineProperty(globalThis, 'localStorage', descriptor);
      }
    }
  });

  it('falls back to localStorage cache when the ledger plane fails (会话内去重降级)', async () => {
    const { sdk } = createMockSdk();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    // 持久面故障：台账集合查询/写入恒失败
    sdk.data.query.mockImplementation(async (name: string, opts?: { prefix?: string }) => {
      if (name === KANBAN_COLLECTIONS.notifyLedger) {
        throw new Error('数据面不可用（mock）');
      }
      return { items: [] };
    });
    sdk.data.save.mockImplementation(async (name: string) => {
      if (name === KANBAN_COLLECTIONS.notifyLedger) {
        throw new Error('数据面不可用（mock）');
      }
      return { success: true };
    });
    const service = new KanbanService(sdk);
    const board = mkBoard();
    const createOp: KanbanCardOp = {
      id: 'op-create-1', orgId: 'org-1', boardId: board.id, cardId: 'card-1',
      kind: 'create', title: '修崩溃', operatorRootId: 'root-1', createdAt: 1
    };
    const assignOp: KanbanCardOp = {
      id: 'op-assign-1', orgId: 'org-1', boardId: board.id, cardId: 'card-1',
      kind: 'assign', assigneeRootId: 'root-me', operatorRootId: 'root-1', createdAt: 2
    };
    expect(await service.notifyAssignedToMe('org-1', 'root-me', [createOp, assignOp], board.name)).toBe(1);
    // 持久面写失败但缓存已记：同实例（进程内兜底）不重复通知
    expect(await service.notifyAssignedToMe('org-1', 'root-me', [createOp, assignOp], board.name)).toBe(0);
    warn.mockRestore();
  });

  it('degrades silently when messages module is absent', async () => {
    const { sdk } = createMockSdk();
    sdk.messages = undefined;
    const service = new KanbanService(sdk);
    const sent = await service.notifyAssignedToMe('org-1', 'root-me', [], '看板');
    expect(sent).toBe(0);
  });
});
