import { describe, expect, it } from 'vitest';
import {
  RESOLUTION_BADGE_LABELS,
  bindingSignContent,
  buildAssignSummary,
  buildBoardView,
  buildKanbanSignPayload,
  canEditCards,
  canManageBoard,
  canSubmitStatusOp,
  cardOpSignContent,
  defaultBoardColumns,
  deriveCardComments,
  deriveNativeCards,
  deriveStatusKeyFromLog,
  extractWriteSet,
  hashKanbanContent,
  nativeCardBinding,
  boundAffairIds,
  resolutionBadgeFromStates,
  resolveActiveBindings,
  resolveAffairCardColumn,
  resolveNativeCardColumn,
  sortCardsInColumn,
  validateBoardName,
  validateCardDescription,
  validateCardTitle,
  validateColumns,
  validateCommentText,
  type KanbanAffairCard,
  type KanbanBinding,
  type KanbanBoard,
  type KanbanCardOp,
  type KanbanCardView
} from '../model';

const mkBoard = (overrides: Partial<KanbanBoard> = {}): KanbanBoard => ({
  id: 'board-1',
  orgId: 'org-1',
  name: '迭代看板',
  columns: defaultBoardColumns(),
  createdBy: 'root-admin',
  createdAt: 1,
  updatedAt: 1,
  ...overrides
});

const mkOp = (overrides: Partial<KanbanCardOp> = {}): KanbanCardOp => ({
  id: `op_${Math.random().toString(16).slice(2, 10)}`,
  orgId: 'org-1',
  boardId: 'board-1',
  cardId: 'card-1',
  kind: 'create',
  operatorRootId: 'root-1',
  createdAt: 1,
  ...overrides
});

const mkBinding = (overrides: Partial<KanbanBinding> = {}): KanbanBinding => ({
  id: `bind_${Math.random().toString(16).slice(2, 10)}`,
  orgId: 'org-1',
  boardId: 'board-1',
  cardRef: 'affair',
  affairId: 'ab'.repeat(32),
  kind: 'bind',
  boundBy: 'root-1',
  boundAt: 1,
  ...overrides
});

const mkAffairCard = (overrides: Partial<KanbanAffairCard> = {}): KanbanAffairCard => ({
  affairId: 'cd'.repeat(32),
  affairType: 'bug',
  title: '崩溃修复',
  summary: '启动即崩溃',
  statusKey: null,
  resolution: 'open',
  createdAt: 5,
  ...overrides
});

describe('spark-kanban model · 看板与列（§3.2）', () => {
  it('default template is 待分诊→待办→进行中→待验证→完成 with stage statusKeys', () => {
    const columns = defaultBoardColumns();
    expect(columns.map((c) => c.kind)).toEqual(['triage', 'stage', 'stage', 'stage', 'terminal']);
    expect(columns.map((c) => c.title)).toEqual(['待分诊', '待办', '进行中', '待验证', '完成']);
    expect(columns.find((c) => c.id === 'doing')?.statusKey).toBe('doing');
    expect(columns.find((c) => c.kind === 'terminal')?.statusKey).toBeUndefined();
  });

  it('validates column definitions (档三-21：stage 列必须声明 statusKey，必须有 triage 与 terminal)', () => {
    expect(validateColumns(defaultBoardColumns()).ok).toBe(true);
    expect(validateColumns([]).ok).toBe(false);
    expect(
      validateColumns([
        { id: 'a', title: 'A', kind: 'stage', order: 0 },
        { id: 't', title: 'T', kind: 'triage', order: 1 },
        { id: 'd', title: 'D', kind: 'terminal', order: 2 }
      ]).ok
    ).toBe(false); // stage 缺 statusKey
    expect(validateColumns([{ id: 'a', title: 'A', kind: 'stage', order: 0, statusKey: 'x' }]).ok).toBe(false); // 无 triage/terminal
    expect(
      validateColumns([
        { id: 'a', title: 'A', kind: 'triage', order: 0 },
        { id: 'a', title: 'B', kind: 'terminal', order: 1 }
      ]).ok
    ).toBe(false); // id 重复
    expect(
      validateColumns([
        { id: 't', title: 'T', kind: 'triage', order: 0 },
        { id: 'a', title: 'A', kind: 'stage', order: 1, statusKey: 'doing' },
        { id: 'b', title: 'B', kind: 'stage', order: 2, statusKey: 'doing' },
        { id: 'd', title: 'D', kind: 'terminal', order: 3 }
      ]).ok
    ).toBe(false); // statusKey 判重：同一状态值只能映射一列
    expect(
      validateColumns([
        { id: 't', title: 'T', kind: 'triage', order: 0, statusKey: 'doing' },
        { id: 'a', title: 'A', kind: 'stage', order: 1, statusKey: 'doing' },
        { id: 'd', title: 'D', kind: 'terminal', order: 2 }
      ]).ok
    ).toBe(false); // triage 与 stage 之间同样判重
  });

  it('binding sign content uses unambiguous separators (cardRef 自身含冒号)', () => {
    const a = bindingSignContent({ kind: 'bind', cardRef: 'native:a', affairId: 'b1' });
    const b = bindingSignContent({ kind: 'bind', cardRef: 'native:a:b', affairId: '1' });
    expect(a).not.toBe(b);
    expect(bindingSignContent({ kind: 'bind', cardRef: 'affair', affairId: 'x'.repeat(64) })).toBe(
      `bind\naffair\n${'x'.repeat(64)}`
    );
  });

  it('validates board name and card fields', () => {
    expect(validateBoardName('').ok).toBe(false);
    expect(validateBoardName('x'.repeat(41)).ok).toBe(false);
    expect(validateBoardName('迭代看板').ok).toBe(true);
    expect(validateCardTitle('').ok).toBe(false);
    expect(validateCardTitle('t'.repeat(121)).ok).toBe(false);
    expect(validateCardDescription('d'.repeat(5001)).ok).toBe(false);
    expect(validateCommentText('').ok).toBe(false);
    expect(validateCommentText('c'.repeat(2001)).ok).toBe(false);
  });
});

describe('spark-kanban model · 权限（档三-19/§2）', () => {
  it('board management is admin-only; card editing is all members (个人空间由 service 放行)', () => {
    expect(canManageBoard('admin')).toBe(true);
    expect(canManageBoard('member')).toBe(false);
    expect(canManageBoard(null)).toBe(false);
    expect(canEditCards('admin')).toBe(true);
    expect(canEditCards('member')).toBe(true);
    expect(canEditCards(null)).toBe(false);
  });

  it('status op permission = write-set membership, fail-closed on empty set', () => {
    const id = 'a'.repeat(64);
    expect(canSubmitStatusOp(new Set([id]), id)).toBe(true);
    expect(canSubmitStatusOp(new Set([id]), 'b'.repeat(64))).toBe(false);
    expect(canSubmitStatusOp(new Set(), id)).toBe(false);
    expect(canSubmitStatusOp(new Set([id]), null)).toBe(false);
  });

  it('extractWriteSet reads rules.maintainers as 64-hex identity list (fail-closed)', () => {
    const valid = 'a'.repeat(64);
    expect(extractWriteSet({ maintainers: [valid, 'not-hex', 42] })).toEqual([valid]);
    expect(extractWriteSet({})).toEqual([]);
    expect(extractWriteSet(null)).toEqual([]);
    expect(extractWriteSet('junk')).toEqual([]);
  });
});

describe('spark-kanban model · 原生卡片操作流折叠（append-only 派生量）', () => {
  it('folds create/move/assign/comment into card state (deterministic createdAt+id order)', () => {
    const ops: KanbanCardOp[] = [
      mkOp({ id: 'op-3', kind: 'move', columnId: 'doing', createdAt: 3 }),
      mkOp({ id: 'op-1', kind: 'create', title: '修 bug', description: '详述', columnId: 'triage', createdAt: 1 }),
      mkOp({ id: 'op-2', kind: 'assign', assigneeRootId: 'root-2', createdAt: 2 }),
      mkOp({ id: 'op-4', kind: 'comment', text: '在看', createdAt: 4 })
    ];
    const cards = deriveNativeCards('board-1', ops);
    expect(cards).toHaveLength(1);
    expect(cards[0]).toMatchObject({
      cardId: 'card-1',
      title: '修 bug',
      columnId: 'doing',
      assigneeRootId: 'root-2',
      commentCount: 1
    });
  });

  it('ignores duplicate create and orphan ops (append-only 脏数据读侧自证)', () => {
    const ops: KanbanCardOp[] = [
      mkOp({ id: 'op-1', kind: 'create', title: '首版', createdAt: 1 }),
      mkOp({ id: 'op-2', kind: 'create', title: '重复建卡', createdAt: 2 }),
      mkOp({ id: 'op-3', kind: 'move', cardId: 'ghost', columnId: 'done', createdAt: 3 })
    ];
    const cards = deriveNativeCards('board-1', ops);
    expect(cards).toHaveLength(1);
    expect(cards[0].title).toBe('首版');
  });

  it('tie-breaks same-timestamp ops by id for cross-device determinism', () => {
    const ops: KanbanCardOp[] = [
      mkOp({ id: 'op-b', kind: 'move', columnId: 'doing', createdAt: 5 }),
      mkOp({ id: 'op-a', kind: 'move', columnId: 'todo', createdAt: 5 }),
      mkOp({ id: 'op-0', kind: 'create', title: 't', createdAt: 1 })
    ];
    // 同刻按 id 字典序：op-a 先、op-b 后 → 最终 doing
    expect(deriveNativeCards('board-1', ops)[0].columnId).toBe('doing');
  });

  it('derives comment timeline oldest-first', () => {
    const ops: KanbanCardOp[] = [
      mkOp({ id: 'op-2', kind: 'comment', text: '后', createdAt: 2 }),
      mkOp({ id: 'op-1', kind: 'create', title: 't', createdAt: 0 }),
      mkOp({ id: 'op-3', kind: 'comment', text: '早', createdAt: 1 })
    ];
    const comments = deriveCardComments('board-1', 'card-1', ops);
    expect(comments.map((c) => c.text)).toEqual(['早', '后']);
  });
});

describe('spark-kanban model · 绑定记录折叠（§3.4/档三-20）', () => {
  it('resolves active bindings by latest record (unbind = 追加记录，历史可考)', () => {
    const records: KanbanBinding[] = [
      mkBinding({ id: 'b1', kind: 'bind', boundAt: 1 }),
      mkBinding({ id: 'b2', kind: 'unbind', boundAt: 2 }),
      mkBinding({ id: 'b3', kind: 'bind', boundAt: 3 })
    ];
    const active = resolveActiveBindings('board-1', records);
    expect(active).toHaveLength(1);
    expect(active[0].id).toBe('b3');
    expect(resolveActiveBindings('board-1', records.slice(0, 2))).toHaveLength(0);
  });

  it('native card → single affair (档三-20)；affair → multi-board allowed', () => {
    const records: KanbanBinding[] = [
      mkBinding({ id: 'b1', cardRef: 'native:card-1', affairId: 'a'.repeat(64), boundAt: 1 }),
      mkBinding({ id: 'b2', cardRef: 'affair', affairId: 'a'.repeat(64), boardId: 'board-1', boundAt: 2 }),
      mkBinding({ id: 'b3', cardRef: 'affair', affairId: 'a'.repeat(64), boardId: 'board-2', boundAt: 3 })
    ];
    expect(nativeCardBinding('board-1', 'card-1', records)?.affairId).toBe('a'.repeat(64));
    // 同一子事务挂两个看板皆生效
    expect(boundAffairIds('board-1', records)).toEqual(['a'.repeat(64)]);
    expect(boundAffairIds('board-2', records)).toEqual(['a'.repeat(64)]);
  });
});

describe('spark-kanban model · 子事务状态推导（档三-1/§3.4；因果序 + 读侧写权过滤）', () => {
  const WRITER = 'a'.repeat(64);
  const OUTSIDER = 'b'.repeat(64);
  const WRITE_SET = new Set([WRITER]);
  const AFFAIR = 'f'.repeat(64);
  const statusOp = (
    status: string,
    opHash: string,
    prevOpHash: string,
    declaredAt = 1,
    actor = WRITER
  ) => ({
    opHash,
    op: {
      opV: 1,
      affairId: AFFAIR,
      opType: 'content',
      prevOpHash,
      payload: { kind: 'kanban.status', status },
      actor: { kind: 'person', identity: actor, publicKey: 'cGsx' },
      declaredAt
    }
  });

  it('orders by causality: descendant wins regardless of declaredAt (协议 §7.2-4 自报时刻不入判定)', () => {
    // 链：创世 → A(todo) → B(doing)；B 声明时刻更早仍胜（因果闭包含 A）
    const ops = [statusOp('doing', 'bb', 'aa', 5), statusOp('todo', 'aa', AFFAIR, 99)];
    expect(deriveStatusKeyFromLog(ops, WRITE_SET)).toBe('doing');
  });

  it('future-dated forgery does NOT win ordering（硬伤 1 回归：declaredAt 远未来挂在创世上的伪造操作输给真实后继）', () => {
    const forged = statusOp('verifying', 'ff', AFFAIR, Number.MAX_SAFE_INTEGER); // 未来远日 + 无因果后继关系
    const real = statusOp('todo', '11', AFFAIR, 1);
    const realNext = statusOp('doing', '22', '11', 2);
    // 输入乱序也不影响推导
    expect(deriveStatusKeyFromLog([forged, realNext, real], WRITE_SET)).toBe('doing');
  });

  it('breaks concurrent branches by opHash lexicographic order（§8 排序键 tie-break）', () => {
    // A、B 同挂创世（并发分支），opHash 大者胜；与输入顺序无关
    const a = statusOp('todo', 'aa', AFFAIR);
    const b = statusOp('doing', 'bb', AFFAIR);
    expect(deriveStatusKeyFromLog([a, b], WRITE_SET)).toBe('doing');
    expect(deriveStatusKeyFromLog([b, a], WRITE_SET)).toBe('doing');
  });

  it('ignores ops from actors outside the write set（硬伤 2 回归：伪造操作不改变推导状态）', () => {
    const forged = statusOp('done', 'ff', 'aa', 50, OUTSIDER); // 集合外 actor 的后继伪造
    const real = statusOp('todo', 'aa', AFFAIR, 1);
    expect(deriveStatusKeyFromLog([real, forged], WRITE_SET)).toBe('todo');
    // 空集合 = 全部不算（与写侧 fail-closed 对称）
    expect(deriveStatusKeyFromLog([real], new Set())).toBeNull();
  });

  it('ignores malformed ops without actor (读侧自证，不脑补语义)', () => {
    const noActor = {
      opHash: 'aa',
      op: { opV: 1, opType: 'content', prevOpHash: AFFAIR, payload: { kind: 'kanban.status', status: 'todo' } }
    };
    expect(deriveStatusKeyFromLog([noActor], WRITE_SET)).toBeNull();
  });

  it('returns null when no status op (新回流子事务 → 待分诊)', () => {
    expect(deriveStatusKeyFromLog([], WRITE_SET)).toBeNull();
    expect(
      deriveStatusKeyFromLog(
        [{ opHash: 'aa', op: { opType: 'content', payload: { kind: 'comment' }, actor: { identity: WRITER } } }],
        WRITE_SET
      )
    ).toBeNull();
  });

  it('maps resolution states to badges (公示期 = 待确认，不提前「已完成」)', () => {
    expect(resolutionBadgeFromStates(['effective', 'pending'])).toBe('effective');
    expect(resolutionBadgeFromStates(['pending'])).toBe('pending');
    expect(resolutionBadgeFromStates(['unanchored'])).toBe('pending');
    expect(resolutionBadgeFromStates(['vetoed'])).toBe('vetoed');
    expect(resolutionBadgeFromStates([])).toBe('open');
    expect(RESOLUTION_BADGE_LABELS.pending).toBe('待确认');
  });

  it('resolves column: effective → terminal; statusKey → mapped column; else triage', () => {
    const columns = defaultBoardColumns();
    expect(resolveAffairCardColumn(columns, 'doing', 'open')).toBe('doing');
    expect(resolveAffairCardColumn(columns, null, 'open')).toBe('triage');
    expect(resolveAffairCardColumn(columns, null, 'pending')).toBe('triage'); // 公示期不提前进终态
    expect(resolveAffairCardColumn(columns, 'doing', 'effective')).toBe('done');
    expect(resolveAffairCardColumn(columns, 'unknown-key', 'open')).toBe('triage'); // 映射未命中回落待分诊
  });

  it('native card in deleted column falls back to triage (列定义变更不丢卡)', () => {
    const columns = defaultBoardColumns();
    expect(resolveNativeCardColumn(columns, 'doing')).toBe('doing');
    expect(resolveNativeCardColumn(columns, 'removed')).toBe('triage');
  });
});

describe('spark-kanban model · 看板组装与列内排序（档三-22 local 偏好）', () => {
  const nativeView = (cardId: string, createdAt: number): KanbanCardView => ({
    kind: 'native',
    ref: `native:${cardId}`,
    columnId: 'todo',
    card: {
      cardId,
      boardId: 'board-1',
      title: cardId,
      description: '',
      columnId: 'todo',
      createdBy: 'root-1',
      createdAt,
      updatedAt: createdAt,
      commentCount: 0,
      moveCount: 0,
      signed: false
    }
  });

  it('sorts by prefs order first, then createdAt for the rest', () => {
    const cards = [nativeView('a', 1), nativeView('b', 2), nativeView('c', 3)];
    const sorted = sortCardsInColumn(cards, ['native:c', 'native:a']);
    expect(sorted.map((v) => v.ref)).toEqual(['native:c', 'native:a', 'native:b']);
    expect(sortCardsInColumn(cards, undefined).map((v) => v.ref)).toEqual(['native:a', 'native:b', 'native:c']);
  });

  it('assembles board view: native + affair cards bucketed, triage freshCount for un-triaged', () => {
    const board = mkBoard();
    const ops: KanbanCardOp[] = [mkOp({ id: 'op-1', kind: 'create', title: '原生卡', columnId: 'triage', createdAt: 1 })];
    const native = deriveNativeCards('board-1', ops);
    const affairs = [
      mkAffairCard({ affairId: 'c'.repeat(64), statusKey: null, resolution: 'open' }),
      mkAffairCard({ affairId: 'd'.repeat(64), statusKey: 'doing', resolution: 'pending' }),
      mkAffairCard({ affairId: 'e'.repeat(64), statusKey: 'doing', resolution: 'effective' })
    ];
    const view = buildBoardView(board, native, affairs, null);
    const byId = new Map(view.map((column) => [column.column.id, column]));
    expect(byId.get('triage')?.cards.map((c) => c.ref)).toEqual(['native:card-1', `affair:${'c'.repeat(64)}`]);
    // U2：未分诊 = 无状态且无决议的绑定卡片 + 建卡后从未 move 的新原生卡片
    expect(byId.get('triage')?.freshCount).toBe(2);
    expect(byId.get('doing')?.cards[0].ref).toBe(`affair:${'d'.repeat(64)}`);
    expect(byId.get('done')?.cards[0].ref).toBe(`affair:${'e'.repeat(64)}`);
  });

  it('freshCount drops a native card once it has been moved (分诊完成不计未分诊)', () => {
    const board = mkBoard();
    const ops: KanbanCardOp[] = [
      mkOp({ id: 'op-1', kind: 'create', title: '原生卡', columnId: 'triage', createdAt: 1 }),
      mkOp({ id: 'op-2', kind: 'move', columnId: 'todo', createdAt: 2 })
    ];
    const view = buildBoardView(board, deriveNativeCards('board-1', ops), [], null);
    expect(view.find((column) => column.column.id === 'triage')?.freshCount).toBe(0);
  });
});

describe('spark-kanban model · 签名载荷与通知摘要', () => {
  it('sign payload binds orgId/recordId/operator/content-hash (四元绑定)', () => {
    const a = buildKanbanSignPayload('org-1', 'op-1', 'root-1', '内容');
    expect(a).toBe(`org-1:op-1:root-1:${hashKanbanContent('内容')}`);
    expect(buildKanbanSignPayload('org-2', 'op-1', 'root-1', '内容')).not.toBe(a);
    expect(hashKanbanContent('内容')).toMatch(/^[0-9a-f]{8}$/);
  });

  it('cardOpSignContent covers each op kind (验签侧同函数重算)', () => {
    expect(cardOpSignContent({ kind: 'create', cardId: 'c1', title: 't', description: 'd' })).toBe('create:c1:t\nd');
    expect(cardOpSignContent({ kind: 'move', cardId: 'c1', columnId: 'doing' })).toBe('move:c1:doing');
    expect(cardOpSignContent({ kind: 'assign', cardId: 'c1', assigneeRootId: 'r1' })).toBe('assign:c1:r1');
    expect(cardOpSignContent({ kind: 'comment', cardId: 'c1', text: 'x' })).toBe('comment:c1:x');
  });

  it('assign summary is self-contained and ≤200 chars (档三-12 最少事件集)', () => {
    const summary = buildAssignSummary('迭代看板', 'x'.repeat(500));
    expect(summary.startsWith('【看板指派·迭代看板】')).toBe(true);
    expect(summary.length).toBeLessThanOrEqual(200);
  });
});
