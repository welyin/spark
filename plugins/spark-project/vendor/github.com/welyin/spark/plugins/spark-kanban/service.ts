/**
 * 任务看板插件（spark-kanban）· 业务服务层（kanban.md v0.2 + 拍板口径）。
 *
 * 职责边界：
 * - 呈现与编排数据走 sdk.data（P6 声明式，写库即同步；组织空间组织内同步、
 *   个人空间自设备间同步，由内核按运行空间处理）：
 *   boards（lww-record/sync，列定义与列—状态映射，档三-21）、
 *   card-ops（append-only/sync，原生卡片操作流）、
 *   bindings（append-only/sync，卡片 ↔ 子事务绑定/解绑，档三-20）、
 *   view-prefs（lww-record/local，列内手动排序，档三-22 不进同步流量）；
 * - 权威层走 sdk.affairs：子事务聚合（listFollowed + readLog 创世 refs parent
 *   匹配 = 自动绑定主路径，反馈回流对看板零成本）、状态操作提交（submitOp，
 *   签名入事务日志，档三-1）、决议读取（readResolution，公示期诚实边界）；
 * - 状态操作权限（档三-19 MVP = 写权集合成员）：提交前取子事务 parent 项目
 *   议题规则文档的 rules.maintainers，当前插件域身份 ∉ 集合即 fail-closed
 *   拒绝并如实说明（与 spark-git-repo 终态写权同约定；内核侧门槛算术把关，
 *   本层是客户端前置拦截，不替代内核判定）；
 * - 通知（档三-12 最少事件集）：只做「被指派」——assign 操作经同步到达本机后
 *   本地生成应用消息（服务号模型 §20.4.3），localStorage 台账去重；
 * - 能力缺失一律降级不报错：无 sdk.affairs（独立使用/移动端只读形态）→
 *   绑定相关入口隐藏、看板降级为纯原生卡片模式（§4 降级纪律）。
 *
 * 签名主体诚实口径（同 spark-affairs）：事务操作 actor 为本插件域身份，
 * 不代表操作者个人身份。
 */
import type {
  AffairChangeEvent,
  AffairOpStatus,
  PluginAffairsAPI,
  PluginSDK
} from '../../packages/plugin-sdk/src';
import {
  buildAssignSummary,
  buildKanbanSignPayload,
  boundAffairIds,
  canEditCards,
  canManageBoard,
  canSubmitStatusOp,
  cardOpSignContent,
  defaultBoardColumns,
  deriveStatusKeyFromLog,
  extractWriteSet,
  normalizeKanbanText,
  resolutionBadgeFromStates,
  resolveActiveBindings,
  validateBoardName,
  validateCardDescription,
  validateCardTitle,
  validateColumns,
  validateCommentText,
  type KanbanAffairCard,
  type KanbanBinding,
  type KanbanBoard,
  type KanbanCardOp,
  type KanbanColumn,
  type KanbanSignature,
  type KanbanViewPrefs
} from './model';
import { buildOpDraft, buildStatusOpPayload, deriveIdentity, signPayload, type AffairActor } from './wire';

/**
 * 集合名工厂（§3.5）。内核 plugindata 强制集合名前缀 == 调用方插件 id，
 * 故前缀不能硬编码：独立安装形态 = 'spark-kanban'（缺省）；被「项目」等
 * 插件构建期组合（库包形态，§5.1）时组合者须以其自身插件 id 构造
 * KanbanService（如 namespace='spark-project'），数据落到组合者命名空间——
 * 库无自己的数据域（sdk.domain 为组合者域）。
 */
export type KanbanCollections = {
  boards: string;
  cardOps: string;
  bindings: string;
  viewPrefs: string;
};

export function kanbanCollections(namespace: string): KanbanCollections {
  return {
    boards: `${namespace}:boards`,
    cardOps: `${namespace}:card-ops`,
    bindings: `${namespace}:bindings`,
    viewPrefs: `${namespace}:view-prefs`
  };
}

/** 独立安装形态的缺省集合名（卡片视图等本插件域上下文使用） */
export const KANBAN_COLLECTIONS: KanbanCollections = kanbanCollections('spark-kanban');

/** 本插件用到的 sdk.affairs 方法核对清单（fail-fast 点名缺失，同 spark-feedback 纪律） */
export const REQUIRED_AFFAIRS_METHODS = [
  'listFollowed',
  'readLog',
  'readResolution',
  'readRules',
  'submitOp',
  'onChange'
] as const;

export const AFFAIRS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.affairs（事务模块）——子事务绑定与状态操作不可用，看板降级为纯原生卡片模式。';

function hasAffairs(mod: PluginSDK['affairs']): mod is PluginAffairsAPI {
  return Boolean(
    mod &&
      REQUIRED_AFFAIRS_METHODS.every(
        (method) => typeof (mod as unknown as Record<string, unknown>)[method] === 'function'
      )
  );
}

/**
 * 记录 id：时间戳 + 进程内单调计数 + 随机后缀。单调计数保证同设备同毫秒
 * 连续写入（如 bind 紧随 unbind）的 id 字典序与写入序一致——append-only
 * 折叠的 createdAt+id tie-break 因此在本机操作序列上稳定（跨设备同毫秒
 * 冲突的胜出者仍是确定性但任意的，append-only 下属可接受语义）。
 */
let idCounter = 0;

function newId(prefix: string): string {
  idCounter = (idCounter + 1) & 0xffffff;
  return `${prefix}_${Date.now()}_${idCounter.toString(16).padStart(6, '0')}_${Math.random().toString(16).slice(2, 10)}`;
}

type KanbanRole = 'admin' | 'member' | null | undefined;

/** 「已通知指派」去重台账（localStorage；通知本地生成本地消费，去重即本机状态） */
const NOTIFIED_KEY_PREFIX = 'spark-kanban:notified-assign:';
const memoryNotifiedFallback = new Map<string, Set<string>>();

function loadNotifiedIds(orgId: string): Set<string> {
  const key = `${NOTIFIED_KEY_PREFIX}${orgId}`;
  try {
    const raw = globalThis.localStorage?.getItem(key);
    if (raw) {
      return new Set(JSON.parse(raw) as string[]);
    }
  } catch {
    /* 存储不可用/数据损坏：进程内兜底 */
  }
  return new Set(memoryNotifiedFallback.get(key) ?? []);
}

function saveNotifiedIds(orgId: string, ids: Set<string>): void {
  const key = `${NOTIFIED_KEY_PREFIX}${orgId}`;
  memoryNotifiedFallback.set(key, new Set(ids));
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify([...ids]));
  } catch {
    /* 进程内兜底已记录 */
  }
}

export class KanbanService {
  private readonly affairs: PluginAffairsAPI | null;
  /** 集合名（按命名空间构造；被组合时 = 组合者插件 id，见 kanbanCollections） */
  private readonly collections: KanbanCollections;
  private collectionsReady: Promise<void> | null = null;
  /** 本插件域身份 actor（首次写操作时经 identity.sign 取回公钥后缓存） */
  private actor: AffairActor | null = null;

  constructor(
    private readonly sdk: PluginSDK,
    namespace = 'spark-kanban'
  ) {
    this.affairs = hasAffairs(sdk.affairs) ? sdk.affairs : null;
    this.collections = kanbanCollections(namespace);
  }

  /** 事务模块可用性（视图层据此隐藏绑定相关入口，§4 降级纪律） */
  get affairsAvailable(): boolean {
    return this.affairs !== null;
  }

  /** 本机操作者身份（插件域身份 id；尚无写操作时为 null） */
  get viewerIdentity(): string | null {
    return this.actor?.identity ?? null;
  }

  // ------------------------------------------------------------------
  // 集合声明与读写（sdk.data；写入前必须声明，声明幂等）
  // ------------------------------------------------------------------

  private ensureCollections(): Promise<void> {
    this.collectionsReady ??= (async () => {
      await this.sdk.data.declareCollection({ name: this.collections.boards, merge: 'lww-record', scope: 'sync' });
      await this.sdk.data.declareCollection({ name: this.collections.cardOps, merge: 'append-only', scope: 'sync' });
      await this.sdk.data.declareCollection({ name: this.collections.bindings, merge: 'append-only', scope: 'sync' });
      // 列内手动排序 = local 视图偏好（档三-22），不进同步流量
      await this.sdk.data.declareCollection({ name: this.collections.viewPrefs, merge: 'lww-record', scope: 'local' });
    })();
    return this.collectionsReady;
  }

  /** 远端合入本插件集合时的回调（sdk.data.onChange；视图层借此重载收敛） */
  async subscribeDataChanges(handler: () => void): Promise<void> {
    if (typeof this.sdk.data.onChange !== 'function') {
      return;
    }
    await this.sdk.data.onChange(handler);
  }

  private async saveRecord(collection: string, key: string, value: unknown): Promise<void> {
    await this.ensureCollections();
    await this.sdk.data.save(collection, key, value);
  }

  // ------------------------------------------------------------------
  // 看板配置（boards：列定义 + 列—状态映射的载体，档三-21）
  // ------------------------------------------------------------------

  async loadBoards(orgId: string): Promise<KanbanBoard[]> {
    await this.ensureCollections();
    const response = await this.sdk.data.query<KanbanBoard>(this.collections.boards, {
      prefix: `${orgId}/`,
      limit: 200
    });
    return response.items
      .map((item) => item.value)
      .filter((board) => board && board.orgId === orgId && Array.isArray(board.columns))
      .sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : 1));
  }

  async getBoard(orgId: string, boardId: string): Promise<KanbanBoard | null> {
    await this.ensureCollections();
    return this.sdk.data.get<KanbanBoard>(this.collections.boards, `${orgId}/${boardId}`);
  }

  /** 创建看板（默认列模板 §3.2；contextAffairId 传入即聚合该项目子事务） */
  async createBoard(
    orgId: string,
    rootId: string,
    input: { name: string; contextAffairId?: string },
    role: KanbanRole
  ): Promise<KanbanBoard> {
    if (!canManageBoard(role) && orgId !== 'personal') {
      throw new Error('仅组织管理员可以创建看板');
    }
    const nameCheck = validateBoardName(input.name);
    if (!nameCheck.ok) {
      throw new Error(nameCheck.reason);
    }
    const columns = defaultBoardColumns();
    const now = Date.now();
    const board: KanbanBoard = {
      id: newId('board'),
      orgId,
      name: normalizeKanbanText(input.name),
      ...(input.contextAffairId?.trim() ? { contextAffairId: input.contextAffairId.trim() } : {}),
      columns,
      createdBy: rootId,
      createdAt: now,
      updatedAt: now
    };
    await this.saveRecord(this.collections.boards, `${orgId}/${board.id}`, board);
    return board;
  }

  /** 更新看板（改名 / 换列定义；lww 覆盖语义符合「当前生效配置」直觉） */
  async updateBoard(
    board: KanbanBoard,
    patch: { name?: string; columns?: KanbanColumn[] },
    role: KanbanRole
  ): Promise<KanbanBoard> {
    if (!canManageBoard(role) && board.orgId !== 'personal') {
      throw new Error('仅组织管理员可以修改看板');
    }
    if (patch.name !== undefined) {
      const check = validateBoardName(patch.name);
      if (!check.ok) {
        throw new Error(check.reason);
      }
    }
    if (patch.columns !== undefined) {
      const check = validateColumns(patch.columns);
      if (!check.ok) {
        throw new Error(check.reason);
      }
    }
    const updated: KanbanBoard = {
      ...board,
      name: patch.name !== undefined ? normalizeKanbanText(patch.name) : board.name,
      columns: patch.columns ?? board.columns,
      updatedAt: Date.now()
    };
    await this.saveRecord(this.collections.boards, `${board.orgId}/${board.id}`, updated);
    return updated;
  }

  // ------------------------------------------------------------------
  // 原生卡片操作流（card-ops，append-only；状态是派生量）
  // ------------------------------------------------------------------

  /**
   * 域身份签名（identity:sign，防抵赖增强能力）。与 spark-forum 同口径：
   * 授权被拒/不可用时不阻断主流程（记录照写，少「已签名」徽标）。
   */
  private async signRecord(
    orgId: string,
    recordId: string,
    operatorRootId: string,
    content: string
  ): Promise<KanbanSignature | null> {
    const payload = buildKanbanSignPayload(orgId, recordId, operatorRootId, content);
    try {
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-kanban] 签名被拒或不可用，记录将不带签名徽标：', error);
      return null;
    }
  }

  /** 验签（identity.verify 免权限）：重算载荷比对 + 密码学验签（同 forum 口径） */
  private async verifySignature(
    orgId: string,
    recordId: string,
    operatorRootId: string,
    content: string,
    signature: KanbanSignature | undefined
  ): Promise<boolean> {
    if (!signature) {
      return false;
    }
    const expected = buildKanbanSignPayload(orgId, recordId, operatorRootId, content);
    if (signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, signature.signature, signature.publicKey);
    return result.valid;
  }

  verifyCardOpSignature(op: KanbanCardOp): Promise<boolean> {
    return this.verifySignature(op.orgId, op.id, op.operatorRootId, cardOpSignContent(op), op.signature);
  }

  verifyBindingSignature(binding: KanbanBinding): Promise<boolean> {
    return this.verifySignature(
      binding.orgId,
      binding.id,
      binding.boundBy,
      `${binding.kind}:${binding.cardRef}:${binding.affairId}`,
      binding.signature
    );
  }

  private async appendCardOp(
    orgId: string,
    rootId: string,
    draft: Omit<KanbanCardOp, 'id' | 'orgId' | 'operatorRootId' | 'createdAt' | 'signature'>,
    role: KanbanRole
  ): Promise<KanbanCardOp> {
    if (!canEditCards(role) && orgId !== 'personal') {
      throw new Error('仅组织成员可以操作看板卡片');
    }
    const op: KanbanCardOp = {
      ...draft,
      id: newId('op'),
      orgId,
      operatorRootId: rootId,
      createdAt: Date.now()
    };
    const signature = await this.signRecord(orgId, op.id, rootId, cardOpSignContent(op));
    if (signature) {
      op.signature = signature;
    }
    await this.saveRecord(this.collections.cardOps, `${orgId}/${op.id}`, op);
    return op;
  }

  /** 建卡（初始列 = 看板待分诊列） */
  async createCard(
    orgId: string,
    rootId: string,
    board: KanbanBoard,
    input: { title: string; description?: string; assigneeRootId?: string },
    role: KanbanRole
  ): Promise<KanbanCardOp> {
    const titleCheck = validateCardTitle(input.title);
    if (!titleCheck.ok) {
      throw new Error(titleCheck.reason);
    }
    const descCheck = validateCardDescription(input.description ?? '');
    if (!descCheck.ok) {
      throw new Error(descCheck.reason);
    }
    const triage = [...board.columns].sort((a, b) => a.order - b.order).find((column) => column.kind === 'triage');
    return this.appendCardOp(
      orgId,
      rootId,
      {
        boardId: board.id,
        cardId: newId('card'),
        kind: 'create',
        columnId: triage?.id ?? board.columns[0]?.id,
        title: normalizeKanbanText(input.title),
        description: normalizeKanbanText(input.description ?? ''),
        assigneeRootId: input.assigneeRootId?.trim() || undefined
      },
      role
    );
  }

  /** 拖动转列（原生卡片：move 操作入 append-only 流；terminal 列 = 归档，视图语义 §3.3） */
  moveCard(orgId: string, rootId: string, boardId: string, cardId: string, columnId: string, role: KanbanRole): Promise<KanbanCardOp> {
    return this.appendCardOp(orgId, rootId, { boardId, cardId, kind: 'move', columnId }, role);
  }

  /** 指派/取消指派（assigneeRootId 空串 = 取消） */
  assignCard(orgId: string, rootId: string, boardId: string, cardId: string, assigneeRootId: string, role: KanbanRole): Promise<KanbanCardOp> {
    return this.appendCardOp(orgId, rootId, { boardId, cardId, kind: 'assign', assigneeRootId }, role);
  }

  async commentCard(orgId: string, rootId: string, boardId: string, cardId: string, text: string, role: KanbanRole): Promise<KanbanCardOp> {
    const check = validateCommentText(text);
    if (!check.ok) {
      throw new Error(check.reason);
    }
    return this.appendCardOp(orgId, rootId, { boardId, cardId, kind: 'comment', text: normalizeKanbanText(text) }, role);
  }

  async loadCardOps(orgId: string): Promise<KanbanCardOp[]> {
    await this.ensureCollections();
    const response = await this.sdk.data.query<KanbanCardOp>(this.collections.cardOps, {
      prefix: `${orgId}/`,
      limit: 2000
    });
    return response.items.map((item) => item.value).filter((op) => op && op.orgId === orgId);
  }

  // ------------------------------------------------------------------
  // 绑定记录（bindings，append-only；解绑 = 追加 unbind，历史可考）
  // ------------------------------------------------------------------

  async loadBindings(orgId: string): Promise<KanbanBinding[]> {
    await this.ensureCollections();
    const response = await this.sdk.data.query<KanbanBinding>(this.collections.bindings, {
      prefix: `${orgId}/`,
      limit: 2000
    });
    return response.items.map((item) => item.value).filter((record) => record && record.orgId === orgId);
  }

  private async appendBinding(
    orgId: string,
    rootId: string,
    draft: Omit<KanbanBinding, 'id' | 'orgId' | 'boundBy' | 'boundAt' | 'signature'>,
    role: KanbanRole
  ): Promise<KanbanBinding> {
    if (!canEditCards(role) && orgId !== 'personal') {
      throw new Error('仅组织成员可以操作绑定关系');
    }
    const binding: KanbanBinding = {
      ...draft,
      id: newId('bind'),
      orgId,
      boundBy: rootId,
      boundAt: Date.now()
    };
    const signature = await this.signRecord(
      orgId,
      binding.id,
      rootId,
      `${binding.kind}:${binding.cardRef}:${binding.affairId}`
    );
    if (signature) {
      binding.signature = signature;
    }
    await this.saveRecord(this.collections.bindings, `${orgId}/${binding.id}`, binding);
    return binding;
  }

  /**
   * 手动绑定原生卡片 → 子事务（升级路径，§3.4 补充路径）。
   * 档三-20：原生卡片 → 单一子事务——已有生效绑定时拒绝改绑（先解绑再绑）。
   */
  async bindNativeCard(
    orgId: string,
    rootId: string,
    boardId: string,
    cardId: string,
    affairId: string,
    existing: KanbanBinding[],
    role: KanbanRole
  ): Promise<KanbanBinding> {
    const normalized = affairId.trim();
    if (!/^[0-9a-f]{64}$/.test(normalized)) {
      throw new Error('子事务 affairId 必须是 64 位小写 hex');
    }
    const active = resolveActiveBindings(boardId, existing).filter((record) => record.cardRef === `native:${cardId}`);
    if (active.length > 0) {
      throw new Error('该卡片已绑定子事务（原生卡片 → 单一子事务，档三-20）；请先解绑再绑定新目标');
    }
    return this.appendBinding(orgId, rootId, { boardId, cardRef: `native:${cardId}`, affairId: normalized, kind: 'bind' }, role);
  }

  /** 显式挂载子事务到本看板（cardRef='affair'；档三-20：子事务 → 多看板允许） */
  async bindAffair(orgId: string, rootId: string, boardId: string, affairId: string, role: KanbanRole): Promise<KanbanBinding> {
    const normalized = affairId.trim();
    if (!/^[0-9a-f]{64}$/.test(normalized)) {
      throw new Error('子事务 affairId 必须是 64 位小写 hex');
    }
    return this.appendBinding(orgId, rootId, { boardId, cardRef: 'affair', affairId: normalized, kind: 'bind' }, role);
  }

  /** 解绑 = 追加 unbind 记录（append-only，历史可考） */
  unbind(orgId: string, rootId: string, binding: KanbanBinding, role: KanbanRole): Promise<KanbanBinding> {
    return this.appendBinding(
      orgId,
      rootId,
      { boardId: binding.boardId, cardRef: binding.cardRef, affairId: binding.affairId, kind: 'unbind' },
      role
    );
  }

  // ------------------------------------------------------------------
  // 视图偏好（view-prefs，lww-record + local；档三-22 列内手动排序不进同步流量）
  // ------------------------------------------------------------------

  async loadViewPrefs(boardId: string): Promise<KanbanViewPrefs | null> {
    await this.ensureCollections();
    return this.sdk.data.get<KanbanViewPrefs>(this.collections.viewPrefs, boardId);
  }

  /** 保存某列的手动排序（合并进既有偏好，其他列不动） */
  async saveCardOrder(boardId: string, columnId: string, refs: string[]): Promise<KanbanViewPrefs> {
    const existing = (await this.loadViewPrefs(boardId)) ?? { boardId, cardOrder: {}, updatedAt: 0 };
    const updated: KanbanViewPrefs = {
      ...existing,
      cardOrder: { ...existing.cardOrder, [columnId]: refs },
      updatedAt: Date.now()
    };
    await this.saveRecord(this.collections.viewPrefs, boardId, updated);
    return updated;
  }

  // ------------------------------------------------------------------
  // 事务集成（权威层；sdk.affairs 缺席 = 独立使用降级，不报错）
  // ------------------------------------------------------------------

  private requireAffairs(): PluginAffairsAPI {
    if (!this.affairs) {
      throw new Error(AFFAIRS_MODULE_MISSING);
    }
    return this.affairs;
  }

  /** 取本插件域身份 actor（一次签名取回公钥推导身份 id，缓存复用；同 spark-affairs 口径） */
  private async ensureActor(): Promise<AffairActor> {
    if (this.actor) {
      return this.actor;
    }
    const probe = await this.sdk.identity.sign('spark-kanban:actor-probe');
    this.actor = {
      kind: 'person',
      identity: deriveIdentity(probe.publicKey),
      publicKey: probe.publicKey
    };
    return this.actor;
  }

  /** 事务操作签名（内核入站硬要求；拒绝授权 = 提交失败上抛，不降级） */
  private async signAffairRecord(record: Record<string, unknown>): Promise<Record<string, unknown>> {
    const signed = await this.sdk.identity.sign(signPayload(record));
    return { ...record, sig: signed.signature };
  }

  /** 订阅事务副本变更（sdk.affairs.onChange；变更通知非可靠队列，handler 内重读收敛） */
  subscribeAffairChanges(handler: (event: AffairChangeEvent) => void): Promise<void> {
    return this.requireAffairs().onChange(handler);
  }

  /**
   * 聚合看板的绑定卡片（子事务）。两条路径（§3.4）：
   * 1. 自动绑定（主路径）：board.contextAffairId 的子事务——本机关注的事务中
   *    创世 refs 含 {target: 项目 affairId, rel: 'parent'} 者自动成为卡片，
   *    新回流反馈（bug 子事务）因此零成本落进待分诊列；
   * 2. 手动绑定（补充路径）：bindings 集合中显式挂上本看板的 affairId
   *    （须本机已关注该事务，否则创世读不到——跳过而非编造占位卡片）。
   *
   * 列位置推导素材：statusKey = 子事务日志内最新有效状态操作（档三-1），
   * resolution = readResolution 徽标（公示期内 pending → 「待确认」）。
   */
  async listAffairCards(board: KanbanBoard, bindings: KanbanBinding[]): Promise<KanbanAffairCard[]> {
    const affairs = this.requireAffairs();
    const followed = await affairs.listFollowed();
    const manuallyBound = new Set(boundAffairIds(board.id, bindings));

    const cards: KanbanAffairCard[] = [];
    for (const affairId of followed) {
      const log = await affairs.readLog(affairId);
      const genesis = log.genesis;
      if (!genesis) {
        // 创世未同步到位（复制未收敛）：跳过而非编造占位卡片（诚实边界）
        continue;
      }
      const isChildOfContext =
        board.contextAffairId !== undefined &&
        Array.isArray(genesis.refs) &&
        genesis.refs.some(
          (ref) =>
            typeof ref === 'object' &&
            ref !== null &&
            (ref as Record<string, unknown>).rel === 'parent' &&
            (ref as Record<string, unknown>).target === board.contextAffairId
        );
      const isManuallyBound = manuallyBound.has(affairId);
      // 项目议题自身不是卡片（只是聚合上下文）
      if (affairId === board.contextAffairId) {
        continue;
      }
      if (!isChildOfContext && !isManuallyBound) {
        continue;
      }
      const resolutions = await affairs.readResolution(affairId);
      // 读侧写权过滤（与写侧 fail-closed 对称）：写权集合取自 parent 项目议题
      // 规则文档；规则不可读 → 空集 = 无人的状态操作被采纳（诚实降级为未分诊）
      const writeSet = await this.writeSetForGenesis(affairId, genesis);
      cards.push({
        affairId,
        affairType: typeof genesis.type === 'string' ? genesis.type : 'affair',
        title: typeof genesis.title === 'string' ? genesis.title : affairId.slice(0, 12),
        summary: typeof genesis.summary === 'string' ? genesis.summary : '',
        statusKey: deriveStatusKeyFromLog(log.ops, writeSet),
        resolution: resolutionBadgeFromStates(resolutions.resolutions.map((item) => item.state)),
        createdAt: typeof genesis.createdAt === 'number' ? genesis.createdAt : 0
      });
    }
    return cards.sort((a, b) => a.createdAt - b.createdAt || (a.affairId < b.affairId ? -1 : 1));
  }

  /** 从创世记录定位 parent 项目议题（写权集合的来源；无 parent → null） */
  private parentAffairOf(genesis: Record<string, unknown> | null): string | null {
    const refs = genesis?.refs;
    if (!Array.isArray(refs)) {
      return null;
    }
    const parent = refs.find(
      (ref) =>
        typeof ref === 'object' &&
        ref !== null &&
        (ref as Record<string, unknown>).rel === 'parent' &&
        typeof (ref as Record<string, unknown>).target === 'string'
    ) as Record<string, unknown> | undefined;
    return (parent?.target as string) ?? null;
  }

  /**
   * 读侧写权集合（非抛出变体）：parent 项目议题（无 parent 回退自身）现行
   * 规则文档的 rules.maintainers；规则不可读 → 空集（fail-closed：无人的
   * 状态操作被采纳，卡片退回未分诊，与 spark-git-repo writeSetOrEmpty 同口径）。
   */
  private async writeSetForGenesis(affairId: string, genesis: Record<string, unknown>): Promise<ReadonlySet<string>> {
    const rulesSource = this.parentAffairOf(genesis) ?? affairId;
    try {
      const rulesView = await this.requireAffairs().readRules(rulesSource);
      return new Set(extractWriteSet(rulesView.current.rules));
    } catch {
      return new Set();
    }
  }

  /**
   * 状态操作写权校验（档三-19 MVP = 写权集合成员）：写权集合取自子事务
   * parent 项目议题的现行规则文档（rules.maintainers）；无 parent 的子事务
   * 回退取其自身规则文档。规则不可读 / 集合为空 / 当前身份 ∉ 集合 →
   * fail-closed 上抛，如实说明（不产出伪造状态操作）。
   */
  private async requireStatusOpMembership(affairId: string): Promise<{ actor: AffairActor; writeSet: ReadonlySet<string> }> {
    const affairs = this.requireAffairs();
    const actor = await this.ensureActor();
    const log = await affairs.readLog(affairId);
    if (!log.genesis) {
      throw new Error(`子事务 ${affairId.slice(0, 12)}… 的创世记录尚未同步到本机，写权校验 fail-closed 中止`);
    }
    const rulesSource = this.parentAffairOf(log.genesis) ?? affairId;
    let writeSet: ReadonlySet<string>;
    try {
      const rulesView = await affairs.readRules(rulesSource);
      writeSet = new Set(extractWriteSet(rulesView.current.rules));
    } catch (error) {
      throw new Error(`无法读取议题规则文档，写权校验 fail-closed 中止：${(error as Error).message}`);
    }
    if (!canSubmitStatusOp(writeSet, actor.identity)) {
      const empty = writeSet.size === 0 ? '（规则文档未声明 maintainers，无人可提交状态操作）' : '';
      throw new Error(
        `状态操作被拒绝：当前身份 ${actor.identity.slice(0, 12)}… 不在议题规则声明的写权集合（rules.maintainers）中${empty}`
      );
    }
    return { actor, writeSet };
  }

  /**
   * 拖动转列 = 向子事务提交签名状态操作（档三-1：中间态唯一权威源）。
   * 提交流程：写权校验（fail-closed）→ prevStatus 取当前推导态 → 构造
   * kanban.status 载荷与操作草稿（prevOpHash = 本地 DAG 头）→ 域身份签名
   * → submitOp（与复制面入站同一校验链）。返回 opHash 与判定状态；
   * status='pending'（未知指向暂存）如实上抛给视图层展示，不伪装成功。
   *
   * 拖入 terminal 列不在此路径：终态 = 子事务决议生效（内核机制），看板
   * MVP 只引导说明，不代为发起决议流程（§3.4/§7 后续项）。
   */
  async submitStatusOp(
    affairId: string,
    statusKey: string,
    note?: string
  ): Promise<{ opHash: string; status: AffairOpStatus }> {
    const { actor, writeSet } = await this.requireStatusOpMembership(affairId);
    const affairs = this.requireAffairs();
    const log = await affairs.readLog(affairId);
    // prevStatus 与读侧同一推导口径（写权过滤 + 因果序），保证捎带值诚实
    const prevStatus = deriveStatusKeyFromLog(log.ops, writeSet);
    const draft = buildOpDraft(
      affairId,
      actor,
      buildStatusOpPayload({ status: statusKey, prevStatus, note }),
      log.heads,
      Date.now()
    );
    const result = await affairs.submitOp(await this.signAffairRecord(draft));
    return { opHash: result.opHash, status: result.status };
  }

  // ------------------------------------------------------------------
  // 通知（档三-12 最少事件集：仅「被指派」；本地生成、本地消费）
  // ------------------------------------------------------------------

  /**
   * 成员侧「本地生成」指派通知（服务号模型 §20.4.3）：assign 操作经同步到达
   * 本机后，指派给当前身份的卡片在本机应用会话生成一条通知（含卡片视图）。
   * localStorage 台账去重；权限被拒/内核限流即中止本轮，未记账的下次补齐。
   */
  async notifyAssignedToMe(orgId: string, myRootId: string, ops: KanbanCardOp[], boardName: string): Promise<number> {
    if (!this.sdk.messages || !myRootId) {
      return 0;
    }
    const notified = loadNotifiedIds(orgId);
    let sent = 0;
    const ordered = [...ops].sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : 1));
    for (const op of ordered) {
      if (op.kind !== 'assign' || op.assigneeRootId !== myRootId || notified.has(op.id)) {
        continue;
      }
      const title = ops.find(
        (candidate) => candidate.kind === 'create' && candidate.cardId === op.cardId
      )?.title;
      try {
        await this.sdk.messages.sendAppMessage(
          { summary: buildAssignSummary(boardName, title ?? op.cardId), cardId: op.cardId, boardId: op.boardId, orgId },
          { viewId: 'card-notify', data: { cardId: op.cardId, boardId: op.boardId, orgId } }
        );
      } catch (error) {
        // 限流/权限降级：本轮放弃，未记账的下轮补齐
        console.warn('[spark-kanban] 指派通知发送失败（权限/限流降级）：', error);
        break;
      }
      notified.add(op.id);
      sent += 1;
    }
    if (sent > 0) {
      saveNotifiedIds(orgId, notified);
    }
    return sent;
  }
}
