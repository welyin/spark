/**
 * 任务看板插件（spark-kanban）· 数据模型与纯函数。
 *
 * 设计依据：wiki/product/bootstrap-plugins/kanban.md v0.2 + decisions.md 拍板口径。
 *
 * 分层总览（§3.1）：看板只持有「呈现与编排」数据（列、排序、绑定关系），
 * 权威状态只有一个来源——
 * - 原生卡片：插件集合内的 append-only 操作流（create/move/assign/comment）；
 * - 绑定卡片：事务子事务本体，列位置由「子事务内签名状态操作」（档三-1）与
 *   决议状态（readResolution，公示期内只显示「待确认」）推导，可从事务日志重推。
 *
 * 拍板落点：
 * - 档三-19：状态操作权限 MVP = 子事务所属项目议题规则写权集合（rules.maintainers）
 *   成员，插件侧提交前 fail-closed 校验（与 spark-git-repo 终态写权同约定）；
 * - 档三-20：子事务 → 多看板允许（绑定记录即视图数据），原生卡片 → 单一子事务；
 * - 档三-21：列—状态映射声明放看板配置（插件集合 KanbanBoard.columns[].statusKey）；
 * - 档三-22：列内手动排序 = local 视图偏好（KanbanViewPrefs.cardOrder），不进同步流量；
 * - 档三-12：通知最少事件集——MVP 只做「被指派」一个事件。
 *
 * 本文件不依赖 SDK 运行时/Vue，全部可单测。
 */

/** 看板名称上限 */
export const KANBAN_MAX_BOARD_NAME_LENGTH = 40;
/** 列标题上限 */
export const KANBAN_MAX_COLUMN_TITLE_LENGTH = 20;
/** 卡片标题上限 */
export const KANBAN_MAX_CARD_TITLE_LENGTH = 120;
/** 卡片描述上限 */
export const KANBAN_MAX_CARD_DESCRIPTION_LENGTH = 5000;
/** 卡片评论上限 */
export const KANBAN_MAX_COMMENT_LENGTH = 2000;
/** 单看板列数上限（视图横向排布的卫生边界） */
export const KANBAN_MAX_COLUMNS = 12;

// ------------------------------------------------------------------
// 看板与列（§3.2；看板配置集合，lww-record + sync）
// ------------------------------------------------------------------

/** 列语义（§3.2）：triage 待分诊 / stage 中间态 / terminal 终态 */
export type KanbanColumnKind = 'triage' | 'stage' | 'terminal';

export type KanbanColumn = {
  id: string;
  title: string;
  kind: KanbanColumnKind;
  /** 列顺序（越小越靠前） */
  order: number;
  /**
   * 列—状态映射（档三-21，声明放看板配置）：本列对应的子事务状态操作值
   * （kanban.status 操作的 payload.status）。triage/terminal 列通常为 undefined——
   * triage = 无状态操作的默认落点，terminal 严格来自决议生效状态。
   */
  statusKey?: string;
};

/** 看板配置记录（kanban_boards，lww-record + sync；档三-21 列定义与映射的载体） */
export type KanbanBoard = {
  id: string;
  /** 组织空间 = orgId；个人空间 = 'personal'（组织业务数据必带归属字段） */
  orgId: string;
  name: string;
  /**
   * 看板上下文（§3.2）：被组合/绑定项目议题时为项目 affairId——以其子事务集合
   * 为绑定卡片数据源（自动绑定主路径）；独立使用为空（纯原生卡片看板）。
   */
  contextAffairId?: string;
  columns: KanbanColumn[];
  createdBy: string;
  createdAt: number;
  updatedAt: number;
};

/**
 * 默认列模板（§3.2 自举场景）：待分诊 → 待办 → 进行中 → 待验证 → 完成。
 * stage 列的 statusKey 即子事务状态操作的目标值，拖动转列 = 提交该值的状态操作。
 */
export function defaultBoardColumns(): KanbanColumn[] {
  return [
    { id: 'triage', title: '待分诊', kind: 'triage', order: 0 },
    { id: 'todo', title: '待办', kind: 'stage', order: 1, statusKey: 'todo' },
    { id: 'doing', title: '进行中', kind: 'stage', order: 2, statusKey: 'doing' },
    { id: 'verifying', title: '待验证', kind: 'stage', order: 3, statusKey: 'verifying' },
    { id: 'done', title: '完成', kind: 'terminal', order: 4 }
  ];
}

// ------------------------------------------------------------------
// 原生卡片操作流（§3.3；kanban_card_ops，append-only + sync）
// ------------------------------------------------------------------

export type KanbanCardOpKind = 'create' | 'move' | 'assign' | 'comment';

/**
 * 签名信息（identity:sign 防抵赖）。与 spark-forum 同口径：签名出自插件域
 * 身份，随记录存储，任何成员可免权限验签（重算载荷比对 + 密码学验签）。
 */
export type KanbanSignature = {
  payload: string;
  signature: string;
  publicKey: string;
};

/**
 * 原生卡片操作（append-only 操作流；卡片状态是派生量，不落库）。
 * - create：建卡（title/description 必填，columnId 缺省 = triage 列）；
 * - move：转列（含拖入 terminal 列 = 归档，视图语义，§3.3）；
 * - assign：指派（assigneeRootId 为空串 = 取消指派）；
 * - comment：评论（text）。
 */
export type KanbanCardOp = {
  id: string;
  orgId: string;
  boardId: string;
  cardId: string;
  kind: KanbanCardOpKind;
  columnId?: string;
  title?: string;
  description?: string;
  assigneeRootId?: string;
  text?: string;
  operatorRootId: string;
  createdAt: number;
  signature?: KanbanSignature;
};

/** 原生卡片派生状态（从操作流折叠，不落库） */
export type KanbanNativeCard = {
  cardId: string;
  boardId: string;
  title: string;
  description: string;
  columnId: string;
  assigneeRootId?: string;
  createdBy: string;
  createdAt: number;
  updatedAt: number;
  commentCount: number;
  /** move 操作数（0 = 建卡后从未转列——triage 列「未分诊」统计依据，§4） */
  moveCount: number;
  signed: boolean;
};

export type KanbanCardComment = {
  opId: string;
  text: string;
  authorRootId: string;
  createdAt: number;
  signature?: KanbanSignature;
};

// ------------------------------------------------------------------
// 绑定记录（§3.4；kanban_bindings，append-only + sync）
// ------------------------------------------------------------------

/**
 * 卡片 ↔ 子事务绑定记录。append-only：解绑 = 追加 kind='unbind' 记录，
 * 历史可考。cardRef 形状：`native:{cardId}`（原生卡片升级为子事务，档三-20
 * 原生卡片 → 单一子事务）或 `affair`（把既有子事务显式挂上本看板，子事务
 * → 多看板允许）。自动绑定（项目子事务聚合）不产生绑定记录——那是读侧推导。
 */
export type KanbanBinding = {
  id: string;
  orgId: string;
  boardId: string;
  cardRef: string;
  affairId: string;
  kind: 'bind' | 'unbind';
  boundBy: string;
  boundAt: number;
  signature?: KanbanSignature;
};

// ------------------------------------------------------------------
// 视图偏好（§3.5；kanban_view_prefs，lww-record + local，档三-22 不进同步流量）
// ------------------------------------------------------------------

export type KanbanViewPrefs = {
  boardId: string;
  /** 列内手动排序：columnId → cardRef 列表（越前越靠上） */
  cardOrder: Record<string, string[]>;
  /** 类型筛选（空数组 = 全部） */
  filterTypes?: string[];
  updatedAt: number;
};

// ------------------------------------------------------------------
// 绑定卡片（子事务）读侧视图
// ------------------------------------------------------------------

/** 子事务卡片（绑定卡片）：本体在事务容器，本结构只是读侧投影 */
export type KanbanAffairCard = {
  affairId: string;
  /** 子事务类型徽标（bug / proposal / pr / 通用，档三-4：未预设类型显示为通用子事务） */
  affairType: string;
  title: string;
  summary: string;
  /** 最新有效状态操作的 status 值（无 = 尚未分诊） */
  statusKey: string | null;
  /** 决议徽标（公示期诚实边界，§3.4/§4） */
  resolution: KanbanResolutionBadge;
  createdAt: number;
};

/** 决议徽标（readResolution 原样呈现口径，不美化） */
export type KanbanResolutionBadge = 'open' | 'pending' | 'effective' | 'vetoed';

export const RESOLUTION_BADGE_LABELS: Record<KanbanResolutionBadge, string> = {
  open: '开放',
  // 公示期内（含未锚定）：显示「待确认」而非「已完成」（community-affairs §3.4 时间语义诚实边界）
  pending: '待确认',
  effective: '已生效',
  vetoed: '已否决'
};

/** 看板视图上的统一卡片投影（原生 / 绑定混排，视觉上以 kind 区分，§3.3） */
export type KanbanCardView =
  | { kind: 'native'; ref: string; columnId: string; card: KanbanNativeCard }
  | { kind: 'affair'; ref: string; columnId: string; card: KanbanAffairCard };

/** 列视图（列头计数 + 卡片列表已按视图偏好排序） */
export type KanbanColumnView = {
  column: KanbanColumn;
  cards: KanbanCardView[];
  /** triage 列的未分诊数（列头醒目提示，§4） */
  freshCount: number;
};

// ------------------------------------------------------------------
// 校验
// ------------------------------------------------------------------

export function normalizeKanbanText(text: string): string {
  return text.trim();
}

export function validateBoardName(name: string): { ok: boolean; reason?: string } {
  const normalized = normalizeKanbanText(name);
  if (!normalized) {
    return { ok: false, reason: '看板名称不能为空' };
  }
  if (normalized.length > KANBAN_MAX_BOARD_NAME_LENGTH) {
    return { ok: false, reason: `看板名称不能超过${KANBAN_MAX_BOARD_NAME_LENGTH}字` };
  }
  return { ok: true };
}

export function validateCardTitle(title: string): { ok: boolean; reason?: string } {
  const normalized = normalizeKanbanText(title);
  if (!normalized) {
    return { ok: false, reason: '卡片标题不能为空' };
  }
  if (normalized.length > KANBAN_MAX_CARD_TITLE_LENGTH) {
    return { ok: false, reason: `卡片标题不能超过${KANBAN_MAX_CARD_TITLE_LENGTH}字` };
  }
  return { ok: true };
}

export function validateCardDescription(description: string): { ok: boolean; reason?: string } {
  if (normalizeKanbanText(description).length > KANBAN_MAX_CARD_DESCRIPTION_LENGTH) {
    return { ok: false, reason: `卡片描述不能超过${KANBAN_MAX_CARD_DESCRIPTION_LENGTH}字` };
  }
  return { ok: true };
}

export function validateCommentText(text: string): { ok: boolean; reason?: string } {
  const normalized = normalizeKanbanText(text);
  if (!normalized) {
    return { ok: false, reason: '评论不能为空' };
  }
  if (normalized.length > KANBAN_MAX_COMMENT_LENGTH) {
    return { ok: false, reason: `评论不能超过${KANBAN_MAX_COMMENT_LENGTH}字` };
  }
  return { ok: true };
}

/** 列定义校验（创建/更新看板配置时）：kind 合法、id 唯一、stage 列须声明 statusKey */
export function validateColumns(columns: KanbanColumn[]): { ok: boolean; reason?: string } {
  if (columns.length === 0) {
    return { ok: false, reason: '看板至少需要一列' };
  }
  if (columns.length > KANBAN_MAX_COLUMNS) {
    return { ok: false, reason: `列数不能超过${KANBAN_MAX_COLUMNS}` };
  }
  const ids = new Set<string>();
  const statusKeys = new Set<string>();
  for (const column of columns) {
    if (!column.id || ids.has(column.id)) {
      return { ok: false, reason: '列 id 不能为空且不能重复' };
    }
    ids.add(column.id);
    if (!normalizeKanbanText(column.title)) {
      return { ok: false, reason: `列「${column.id}」标题不能为空` };
    }
    if (normalizeKanbanText(column.title).length > KANBAN_MAX_COLUMN_TITLE_LENGTH) {
      return { ok: false, reason: `列标题不能超过${KANBAN_MAX_COLUMN_TITLE_LENGTH}字` };
    }
    if (!['triage', 'stage', 'terminal'].includes(column.kind)) {
      return { ok: false, reason: `列「${column.title}」kind 非法（triage/stage/terminal）` };
    }
    if (column.kind === 'stage' && !normalizeKanbanText(column.statusKey ?? '')) {
      return { ok: false, reason: `中间态列「${column.title}」必须声明列—状态映射（statusKey）` };
    }
    // statusKey 判重：同一状态值映射到多列会让落列解析歧义（取首个命中），配置期拒掉
    const statusKey = column.kind !== 'terminal' ? normalizeKanbanText(column.statusKey ?? '') : '';
    if (statusKey) {
      if (statusKeys.has(statusKey)) {
        return { ok: false, reason: `列—状态映射「${statusKey}」重复：同一状态值只能映射到一列` };
      }
      statusKeys.add(statusKey);
    }
  }
  if (!columns.some((column) => column.kind === 'triage')) {
    return { ok: false, reason: '看板必须有一列待分诊列（triage）——新卡片/新回流子事务的默认落点' };
  }
  if (!columns.some((column) => column.kind === 'terminal')) {
    return { ok: false, reason: '看板必须有一列终态列（terminal）' };
  }
  return { ok: true };
}

// ------------------------------------------------------------------
// 权限（视图层入口显隐 + service 层拦截双保险）
// ------------------------------------------------------------------

type KanbanRole = 'admin' | 'member' | null | undefined;

/** 看板管理（创建/改列定义）：组织管理员；个人空间（role=null 且本人数据域）由视图层另行放行 */
export function canManageBoard(role: KanbanRole): boolean {
  return role === 'admin';
}

/** 建卡/拖动/评论/认领：组织成员（独立使用口径，§2 角色表） */
export function canEditCards(role: KanbanRole): boolean {
  return role === 'admin' || role === 'member';
}

/**
 * 状态操作权限（档三-19 MVP = 写权集合成员）：向子事务提交状态操作前，
 * 当前身份须 ∈ 子事务所属项目议题规则文档的写权集合。集合为空 =
 * fail-closed（无人可提交，如实说明而非放行）。
 */
export function canSubmitStatusOp(writeSet: ReadonlySet<string>, identity: string | null | undefined): boolean {
  return Boolean(identity) && writeSet.has(identity as string);
}

/**
 * 项目议题规则文档 → 写权集合（rules.maintainers，64-hex 身份 id 列表）。
 * 与 spark-git-repo extractWriteSet 同一字段约定（插件间不共享代码——插件
 * 唯一依赖 plugin-sdk，此处有意重复一份小函数并以注释标注同源）。
 * 字段缺失/形状非法 → 空集（fail-closed）。
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

// ------------------------------------------------------------------
// 原生卡片操作流折叠（派生量；确定性排序：createdAt + id 字典序 tie-break，
// 跨设备时钟不齐时各端派生一致——同 spark-forum deriveTopicState 口径）
// ------------------------------------------------------------------

function byDeterministicOrder<T extends { createdAt: number; id: string }>(a: T, b: T): number {
  return a.createdAt - b.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** 从操作流折叠原生卡片状态（boardId 过滤 + create 建基 + move/assign 推进 + comment 计数） */
export function deriveNativeCards(boardId: string, ops: KanbanCardOp[]): KanbanNativeCard[] {
  const ordered = ops.filter((op) => op.boardId === boardId).sort(byDeterministicOrder);
  const cards = new Map<string, KanbanNativeCard>();
  for (const op of ordered) {
    const existing = cards.get(op.cardId);
    if (op.kind === 'create') {
      // 同 cardId 重复 create（脏数据）：首条为准，后续忽略（append-only 集合读侧自证）
      if (existing) {
        continue;
      }
      cards.set(op.cardId, {
        cardId: op.cardId,
        boardId: op.boardId,
        title: op.title ?? '',
        description: op.description ?? '',
        columnId: op.columnId ?? 'triage',
        assigneeRootId: op.assigneeRootId || undefined,
        createdBy: op.operatorRootId,
        createdAt: op.createdAt,
        updatedAt: op.createdAt,
        commentCount: 0,
        moveCount: 0,
        signed: Boolean(op.signature)
      });
      continue;
    }
    // move/assign/comment 作用于已存在的卡片；无 create 的操作（脏数据）忽略不丢
    if (!existing) {
      continue;
    }
    if (op.kind === 'move' && op.columnId) {
      existing.columnId = op.columnId;
      existing.moveCount += 1;
      existing.updatedAt = op.createdAt;
    } else if (op.kind === 'assign') {
      existing.assigneeRootId = op.assigneeRootId || undefined;
      existing.updatedAt = op.createdAt;
    } else if (op.kind === 'comment') {
      existing.commentCount += 1;
      existing.updatedAt = op.createdAt;
    }
  }
  return [...cards.values()];
}

/** 卡片评论时间线（旧→新） */
export function deriveCardComments(boardId: string, cardId: string, ops: KanbanCardOp[]): KanbanCardComment[] {
  return ops
    .filter((op) => op.boardId === boardId && op.cardId === cardId && op.kind === 'comment')
    .sort(byDeterministicOrder)
    .map((op) => ({
      opId: op.id,
      text: op.text ?? '',
      authorRootId: op.operatorRootId,
      createdAt: op.createdAt,
      signature: op.signature
    }));
}

// ------------------------------------------------------------------
// 绑定记录折叠（bind/unbind；append-only 读侧派生当前生效集）
// ------------------------------------------------------------------

/** 当前生效的绑定：同一 (boardId, cardRef, affairId) 上最新记录为 bind 才生效 */
export function resolveActiveBindings(boardId: string, records: KanbanBinding[]): KanbanBinding[] {
  const ordered = records.filter((record) => record.boardId === boardId).sort(
    (a, b) => a.boundAt - b.boundAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
  const latestByKey = new Map<string, KanbanBinding>();
  for (const record of ordered) {
    // 折叠键带分隔符：cardRef 自身含 ':'，无分隔符拼合有跨字段歧义
    latestByKey.set(`${record.cardRef}\n${record.affairId}`, record);
  }
  return [...latestByKey.values()].filter((record) => record.kind === 'bind');
}

/** 原生卡片当前绑定的子事务（档三-20：原生卡片 → 单一子事务，取最新生效绑定） */
export function nativeCardBinding(boardId: string, cardId: string, records: KanbanBinding[]): KanbanBinding | null {
  const active = resolveActiveBindings(boardId, records)
    .filter((record) => record.cardRef === `native:${cardId}`)
    // boundAt + id 字典序 tie-break（跨设备时钟不齐时各端一致）
    .sort((a, b) => b.boundAt - a.boundAt || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0));
  return active[0] ?? null;
}

/** 显式挂上本看板的子事务 affairId 集合（cardRef='affair' 路径；自动绑定不在此列） */
export function boundAffairIds(boardId: string, records: KanbanBinding[]): string[] {
  return resolveActiveBindings(boardId, records)
    .filter((record) => record.cardRef === 'affair')
    .map((record) => record.affairId)
    .sort();
}

// ------------------------------------------------------------------
// 子事务状态推导（档三-1：子事务内签名状态操作为中间态唯一权威源；
// 终态严格来自内核决议状态，§3.4）
// ------------------------------------------------------------------

/** 状态操作 kind（opType=content 的插件语义载荷；内核不解释，任何副本可重算） */
export const KANBAN_STATUS_OP_KIND = 'kanban.status';

/**
 * 从事务操作日志提取最新有效状态操作的 status 值（档三-1 权威源推导）。
 *
 * 两条安全纪律（独立评审修复）：
 * - **因果序，不用 declaredAt**：协议 §7.2-4 明确 declaredAt 是签名者自报文本、
 *   永不进入任何判定（含排序）——恶意成员可构造未来远日的状态操作永久赢排序。
 *   故「最新」按 prevOpHash 因果闭包判定：B 的祖先集含 A 则 B 更晚；并发分支
 *   按 opHash 字典序 tie-break（§8 排序键；与内核 core/src/affair/op.rs
 *   ancestor_op_hashes 同构，插件侧纯函数实现）。与输入数组顺序无关：
 *   候选按（因果后继分 = 祖先闭包内候选数，opHash）字典序取最大——因果可比的
 *   对子后继分严格递增，并发对子落入 opHash tie-break，总成全序。
 * - **读侧写权过滤**（与写侧 requireStatusOpMembership 的 fail-closed 对称）：
 *   actor ∉ writeSet 的状态操作不参与推导（append-only 日志挡不住自制客户端
 *   伪造，读侧必须自证；同 spark-git-repo derivePrState 读侧过滤先例）。
 *   writeSet 为空 = 全部不算（规则不可读/未声明时无人可提交，诚实降级）。
 */
export function deriveStatusKeyFromLog(
  ops: Array<{ opHash: string; op: Record<string, unknown> }>,
  writeSet: ReadonlySet<string>
): string | null {
  // prevOpHash 边表覆盖全量操作（因果链经过的非状态操作同样传递先后关系）；
  // prev 指向创世 affairId/未知操作时止步（创世不在操作集内，同内核口径）
  const prevOf = new Map<string, string>();
  for (const entry of ops) {
    const prev = entry.op?.prevOpHash;
    if (typeof prev === 'string') {
      prevOf.set(entry.opHash, prev);
    }
  }

  const candidates: Array<{ opHash: string; status: string }> = [];
  for (const entry of ops) {
    const op = entry.op;
    if (op.opType !== 'content') {
      continue;
    }
    const payload = op.payload as Record<string, unknown> | undefined;
    if (!payload || payload.kind !== KANBAN_STATUS_OP_KIND || typeof payload.status !== 'string') {
      continue;
    }
    const actor = (op.actor as Record<string, unknown> | undefined)?.identity;
    if (typeof actor !== 'string' || !writeSet.has(actor)) {
      // 写权集合外（或缺 actor 的畸形操作）：留痕在日志，不参与推导
      continue;
    }
    candidates.push({ opHash: entry.opHash, status: payload.status });
  }
  if (candidates.length === 0) {
    return null;
  }

  // 祖先闭包（visited 天然防环）：从 root 沿 prevOpHash 回溯可达的操作集
  const ancestorClosure = (root: string): Set<string> => {
    const visited = new Set<string>();
    const stack = [root];
    while (stack.length > 0) {
      const hash = stack.pop() as string;
      if (!visited.add(hash)) {
        continue;
      }
      const prev = prevOf.get(hash);
      if (prev !== undefined && prevOf.has(prev)) {
        stack.push(prev);
      }
    }
    return visited;
  };

  const candidateHashes = new Set(candidates.map((candidate) => candidate.opHash));
  const scored = candidates.map((candidate) => {
    const closure = ancestorClosure(candidate.opHash);
    let score = 0;
    for (const other of closure) {
      if (other !== candidate.opHash && candidateHashes.has(other)) {
        score += 1;
      }
    }
    return { ...candidate, score };
  });
  scored.sort((a, b) => b.score - a.score || (a.opHash < b.opHash ? 1 : a.opHash > b.opHash ? -1 : 0));
  return scored[0].status;
}

/**
 * 决议集合 → 徽标（§4：开放 / 待确认 / 已生效 / 已否决，原样呈现不美化）。
 * effective 优先（已有生效决议即终态）；pending/unanchored → 待确认
 * （公示期内任何副本看到的都应是「待确认决议」）；全部 vetoed → 已否决。
 */
export function resolutionBadgeFromStates(
  states: Array<'pending' | 'effective' | 'vetoed' | 'unanchored'>
): KanbanResolutionBadge {
  if (states.some((state) => state === 'effective')) {
    return 'effective';
  }
  if (states.some((state) => state === 'pending' || state === 'unanchored')) {
    return 'pending';
  }
  if (states.length > 0) {
    return 'vetoed';
  }
  return 'open';
}

/**
 * 绑定卡片列位置推导（§3.4 状态推导的完整口径）：
 * 1. 决议已生效 → 终态列（第一个 terminal 列）——公示期内（pending）不提前
 *    入列，徽标显示「待确认」，列位置仍按状态操作推导；
 * 2. 状态操作 statusKey 命中某 stage/triage 列的映射 → 该列；
 * 3. 无状态操作 / 映射未命中 → 待分诊列（新回流反馈的默认落点，§3.4 自动绑定）。
 */
export function resolveAffairCardColumn(
  columns: KanbanColumn[],
  statusKey: string | null,
  resolution: KanbanResolutionBadge
): string {
  const ordered = [...columns].sort((a, b) => a.order - b.order);
  if (resolution === 'effective') {
    const terminal = ordered.find((column) => column.kind === 'terminal');
    if (terminal) {
      return terminal.id;
    }
  }
  if (statusKey) {
    const mapped = ordered.find((column) => column.kind !== 'terminal' && column.statusKey === statusKey);
    if (mapped) {
      return mapped.id;
    }
  }
  const triage = ordered.find((column) => column.kind === 'triage');
  return (triage ?? ordered[0]).id;
}

/** 原生卡片列位置兜底：列定义变更后卡片落在已删除列 → 归入待分诊列（不丢卡） */
export function resolveNativeCardColumn(columns: KanbanColumn[], columnId: string): string {
  if (columns.some((column) => column.id === columnId)) {
    return columnId;
  }
  const ordered = [...columns].sort((a, b) => a.order - b.order);
  const triage = ordered.find((column) => column.kind === 'triage');
  return (triage ?? ordered[0]).id;
}

// ------------------------------------------------------------------
// 看板组装（列视图 + 列内排序：手动排序 = local 视图偏好叠加，档三-22）
// ------------------------------------------------------------------

/** 列内排序：视图偏好 cardOrder 内的卡片优先按偏好序，其余按创建时刻升序兜底 */
export function sortCardsInColumn(cards: KanbanCardView[], prefsOrder: string[] | undefined): KanbanCardView[] {
  const createdAtOf = (card: KanbanCardView): number =>
    card.kind === 'native' ? card.card.createdAt : card.card.createdAt;
  if (!prefsOrder || prefsOrder.length === 0) {
    return [...cards].sort((a, b) => createdAtOf(a) - createdAtOf(b));
  }
  const rank = new Map(prefsOrder.map((ref, index) => [ref, index]));
  return [...cards].sort((a, b) => {
    const ra = rank.get(a.ref);
    const rb = rank.get(b.ref);
    if (ra !== undefined && rb !== undefined) {
      return ra - rb;
    }
    if (ra !== undefined) {
      return -1;
    }
    if (rb !== undefined) {
      return 1;
    }
    return createdAtOf(a) - createdAtOf(b);
  });
}

/**
 * 看板组装：原生卡片（操作流派生）+ 绑定卡片（子事务推导）按列分桶，
 * 列内应用 local 手动排序偏好。triage 列统计未分诊数（无状态操作且无生效
 * 决议的绑定卡片 + 未经任何 move 的新原生卡片）供列头醒目提示。
 */
export function buildBoardView(
  board: KanbanBoard,
  nativeCards: KanbanNativeCard[],
  affairCards: KanbanAffairCard[],
  prefs?: KanbanViewPrefs | null
): KanbanColumnView[] {
  const ordered = [...board.columns].sort((a, b) => a.order - b.order);
  const buckets = new Map<string, KanbanCardView[]>(ordered.map((column) => [column.id, []]));

  for (const card of nativeCards) {
    const columnId = resolveNativeCardColumn(ordered, card.columnId);
    buckets.get(columnId)?.push({ kind: 'native', ref: `native:${card.cardId}`, columnId, card });
  }
  for (const card of affairCards) {
    const columnId = resolveAffairCardColumn(ordered, card.statusKey, card.resolution);
    buckets.get(columnId)?.push({ kind: 'affair', ref: `affair:${card.affairId}`, columnId, card });
  }

  return ordered.map((column) => {
    const cards = sortCardsInColumn(buckets.get(column.id) ?? [], prefs?.cardOrder?.[column.id]);
    const freshCount =
      column.kind === 'triage'
        ? cards.filter(
            (view) =>
              // 绑定卡片：无状态操作且无决议进展 = 未分诊（新回流反馈落点）
              (view.kind === 'affair' && view.card.statusKey === null && view.card.resolution === 'open') ||
              // 原生卡片：建卡后从未转列 = 未分诊（§4「新卡片未分诊醒目提示」口径）
              (view.kind === 'native' && view.card.moveCount === 0)
          ).length
        : 0;
    return { column, cards, freshCount };
  });
}

// ------------------------------------------------------------------
// 签名载荷与通知摘要
// ------------------------------------------------------------------

/**
 * 内容哈希（FNV-1a 32bit，hex；直接复用 spark-example/spark-forum 的写法）：
 * 只承担签名载荷压缩，防抵赖强度由 Ed25519 域签名保证；插件沙箱不假设
 * WebCrypto 可用（opaque origin iframe），故纯 TS 实现。
 */
export function hashKanbanContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/**
 * 签名载荷四元绑定（同 spark-forum buildForumSignPayload 模式）：
 * `{orgId}:{opId}:{operatorRootId}:{内容哈希}`——签名绑定「谁在哪个空间以哪个
 * 身份写了哪条操作」，验签侧从记录当前字段重算比对，无法剪贴重放。
 */
export function buildKanbanSignPayload(
  orgId: string,
  recordId: string,
  operatorRootId: string,
  content: string
): string {
  return `${orgId}:${recordId}:${operatorRootId}:${hashKanbanContent(content)}`;
}

/** 卡片操作的签名内容（各 kind 的语义字段拼合，验签侧按同一函数重算） */
export function cardOpSignContent(op: Pick<KanbanCardOp, 'kind' | 'cardId'> & Partial<KanbanCardOp>): string {
  switch (op.kind) {
    case 'create':
      return `create:${op.cardId}:${op.title ?? ''}\n${op.description ?? ''}`;
    case 'move':
      return `move:${op.cardId}:${op.columnId ?? ''}`;
    case 'assign':
      return `assign:${op.cardId}:${op.assigneeRootId ?? ''}`;
    case 'comment':
      return `comment:${op.cardId}:${op.text ?? ''}`;
  }
}

/**
 * 绑定记录的签名内容（验签侧按同一函数重算）。
 * 字段间用 \n 分隔：cardRef 自身含 ':'（'native:{cardId}'），冒号分隔会有
 * 拼合歧义（'bind:native:a' + 'b1' 与 'bind:native:a:b' + '1' 同文）；
 * kind/cardRef/affairId 均不含换行，\n 分隔无歧义。
 */
export function bindingSignContent(
  binding: Pick<KanbanBinding, 'kind' | 'cardRef' | 'affairId'>
): string {
  return `${binding.kind}\n${binding.cardRef}\n${binding.affairId}`;
}

/** 「被指派」通知摘要（档三-12 最少事件集；summary 强制 ≤200 字符、自成一体） */
export function buildAssignSummary(boardName: string, cardTitle: string): string {
  const board = normalizeKanbanText(boardName) || '看板';
  const title = normalizeKanbanText(cardTitle);
  const prefix = `【看板指派·${board}】`;
  const budget = Math.max(40, 200 - prefix.length - 1);
  const preview = title.slice(0, budget);
  const ellipsis = title.length > budget ? '…' : '';
  return `${prefix}${preview}${ellipsis}`;
}
