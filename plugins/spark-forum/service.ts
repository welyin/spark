/**
 * 论坛插件（spark-forum）· 业务服务层。
 *
 * 架构纪律（沿用 spark-example §5「服务层集中 SDK 调用」）：所有 SDK 调用
 * 集中在服务层，视图组件不直接碰 sdk.docs——便于单测（tests/ 用 mock SDK
 * 驱动本层）与权限审计（本文件即插件能力面清单）。
 *
 * 本层协作的 SDK 能力：
 *   1) docs（storage:read/write）：集合声明 + 文档读写，同步策略写入前必须声明；
 *   2) identity（identity:sign）：主题/回复/治理事件防抵赖签名，验签免权限；
 *   3) messages（message:app）：新主题应用通知 + 主题卡片——发帖者路径只是
 *      发帖者本机的即时反馈；成员侧按服务号模型（p2p-messages §20.4.3）在
 *      同步后从本机数据「本地生成」通知（见 notifyNewTopics）。
 *
 * 权限降级原则：签名与应用消息是「增强能力」，授权被拒或限流时不阻断主流程
 * （帖子照发），仅少一个徽标/少一条通知。
 *
 * 与 spark-example 的语义差异（forum.md §5 需改造项）：
 * - 发帖权限反转：全员可发主题与回复，管理员权限移到板块管理与主题治理；
 * - 三层结构：查询从「按 orgId 过滤」变为 boardId/topicId 二级过滤 + 状态派生；
 * - 通知口径（档三-12）：回复不通知、仅新主题通知；
 * - 删除诉求（档三-14）：以「关闭 + 隐藏」事件表达，没有真删除。
 */
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  FORUM_QUERY_LIMITS,
  buildForumSignPayload,
  buildTopicSummary,
  canManageBoards,
  canModerateTopics,
  canPostTopic,
  canReplyTopic,
  deriveTopicState,
  normalizeForumText,
  validateBoardInput,
  type ForumBoard,
  type ForumReply,
  type ForumSignature,
  type ForumTopic,
  type ForumTopicEvent,
  type ForumTopicEventKind
} from './model';

export const FORUM_COLLECTIONS = {
  orgConfig: 'forum_org_config',
  boards: 'forum_boards',
  topics: 'forum_topics',
  replies: 'forum_replies',
  topicEvents: 'forum_topic_events'
} as const;

/**
 * 集合同步策略声明（写入前必须声明，启动时统一声明一次）：
 * - orgConfig / boards：组织级配置与板块，可被后续管理员调整覆盖，显式 lww；
 * - topics / replies / topicEvents：内容记录与治理留痕，仅追加、不覆盖、
 *   不删除，append-only（自动链式存证）。topicEvents 按档三-10 拍板用
 *   append-only 即可（内容管理动作非决议），不声明 governance:true。
 */
const FORUM_COLLECTION_SCHEMAS = {
  [FORUM_COLLECTIONS.orgConfig]: { syncStrategy: 'lww' },
  [FORUM_COLLECTIONS.boards]: { syncStrategy: 'lww' },
  [FORUM_COLLECTIONS.topics]: { syncStrategy: 'append-only' },
  [FORUM_COLLECTIONS.replies]: { syncStrategy: 'append-only' },
  [FORUM_COLLECTIONS.topicEvents]: { syncStrategy: 'append-only' }
} as const;

/**
 * 组织级配置（§5 需改造 ensureOrgConfig：沿用单文档 lww，扩字段——板块
 * 初始化标记，避免每次进组织重复播种默认板块）。
 */
export type ForumOrgConfig = {
  orgId: string;
  superAdminRootId: string;
  createdBy: string;
  createdAt: number;
  /** 默认板块是否已播种（幂等标记） */
  boardsSeeded?: boolean;
};

type ForumRole = 'admin' | 'member' | null | undefined;

function newId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

/**
 * 「已通知主题」去重台账（H1 评审修复，对齐 spark-announcement 送达台账范式）：
 * 按空间（orgId）记录本机已为哪些主题 id 生成过应用消息（去重键 = 主题 id，
 * 值携带 createdAt 水位），防重启后重复推通知。应用消息是「本地生成、本地
 * 消费」（§20.4.3）——消息本身不同步，去重状态因此也只须是本机状态。
 *
 * 持久面选型（H1）：插件沙箱 iframe 是 opaque origin（壳层
 * sandbox="allow-scripts"），localStorage 恒抛 SecurityError，且 iframe 随
 * 标签切换销毁重建、进程内兜底一并清零——只用 localStorage 会导致每次打开
 * 插件为全部历史主题重发通知。故台账迁到 sdk.data 持久面：
 * `spark-forum:notified`（declareCollection scope:'local'，键
 * `{orgId}:{topicId}`）；localStorage 仅作缓存/兜底（持久面读写失败时降级），
 * 读侧以持久台账为准。
 */
const NOTIFIED_COLLECTION = 'spark-forum:notified';
const NOTIFIED_KEY_PREFIX = 'spark-forum:notified-topics:';
const memoryNotifiedFallback = new Map<string, Set<string>>();

function notifiedStorageKey(orgId: string): string {
  return `${NOTIFIED_KEY_PREFIX}${orgId}`;
}

/** localStorage 缓存读（含进程内兜底；仅作持久面不可用时的降级来源） */
function loadNotifiedCache(orgId: string): Set<string> {
  const key = notifiedStorageKey(orgId);
  try {
    const raw = globalThis.localStorage?.getItem(key);
    if (raw) {
      return new Set(JSON.parse(raw) as string[]);
    }
  } catch {
    /* opaque origin 沙箱恒抛 SecurityError、隐私模式或数据损坏：走进程内兜底 */
  }
  return new Set(memoryNotifiedFallback.get(key) ?? []);
}

/** localStorage 缓存写（best-effort；进程内兜底先行，存储不可用不阻塞） */
function saveNotifiedCache(orgId: string, ids: Set<string>): void {
  const key = notifiedStorageKey(orgId);
  memoryNotifiedFallback.set(key, new Set(ids));
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify([...ids]));
  } catch {
    /* 存储不可用时进程内兜底已记录，忽略 */
  }
}

export class ForumService {
  private collectionsReady: Promise<void> | null = null;
  private notifiedCollectionReady: Promise<void> | null = null;

  constructor(private readonly sdk: PluginSDK) {}

  /** 声明本插件全部集合的同步策略（幂等，重复声明与首次一致即可） */
  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(FORUM_COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  /** 声明通知台账集合（sdk.data scope:'local'，幂等；代际内策略冲突报错） */
  private ensureNotifiedCollection(): Promise<void> {
    this.notifiedCollectionReady ??= this.sdk.data
      .declareCollection({ name: NOTIFIED_COLLECTION, scope: 'local' })
      .then(() => undefined);
    return this.notifiedCollectionReady;
  }

  /**
   * 读通知台账：以 sdk.data 持久面为准；持久面不可用（声明/查询失败）时降级
   * localStorage 缓存 + 进程内兜底。读成功后回写缓存，供降级路径使用。
   */
  private async loadNotifiedTopicIds(orgId: string): Promise<Set<string>> {
    try {
      await this.ensureNotifiedCollection();
      const response = await this.sdk.data.query<{ createdAt?: number }>(NOTIFIED_COLLECTION, {
        prefix: `${orgId}:`,
        limit: 2000
      });
      const ids = new Set(response.items.map((item) => item.key.slice(orgId.length + 1)));
      saveNotifiedCache(orgId, ids);
      return ids;
    } catch (error) {
      console.warn('[spark-forum] 通知台账持久面读取失败，降级缓存（本次会话内去重）：', error);
      return loadNotifiedCache(orgId);
    }
  }

  /** 记通知台账：持久面为主，缓存兜底同步刷新（持久面写失败不阻塞通知流程） */
  private async markNotified(orgId: string, topic: ForumTopic): Promise<void> {
    const cached = loadNotifiedCache(orgId);
    cached.add(topic.id);
    saveNotifiedCache(orgId, cached);
    try {
      await this.ensureNotifiedCollection();
      await this.sdk.data.save(NOTIFIED_COLLECTION, `${orgId}:${topic.id}`, { createdAt: topic.createdAt });
    } catch (error) {
      console.warn('[spark-forum] 通知台账持久面写入失败（缓存已记，降级为会话内去重）：', error);
    }
  }

  async ensureOrgConfig(orgId: string, rootId: string): Promise<ForumOrgConfig> {
    await this.ensureCollectionsDeclared();
    const existing = await this.sdk.docs.get<ForumOrgConfig>(FORUM_COLLECTIONS.orgConfig, orgId);
    if (existing) {
      return existing;
    }

    const created: ForumOrgConfig = {
      orgId,
      superAdminRootId: rootId,
      createdBy: rootId,
      createdAt: Date.now(),
      boardsSeeded: false
    };

    await this.sdk.docs.put(FORUM_COLLECTIONS.orgConfig, orgId, created as unknown as Record<string, unknown>);
    return created;
  }

  /**
   * 首次进入组织时播种一个默认板块（「综合讨论」），保证新组织有地方发帖。
   * 幂等：经 orgConfig.boardsSeeded 标记，多设备并发首次进入时 lww 覆盖
   * 语义下最坏情况是板块播种两次尝试——用固定 id 使 put 收敛为同一记录。
   */
  async seedDefaultBoard(orgId: string, rootId: string): Promise<ForumBoard> {
    await this.ensureCollectionsDeclared();
    const boardId = `board_default_${orgId}`;
    const existing = await this.sdk.docs.get<ForumBoard>(FORUM_COLLECTIONS.boards, boardId);
    if (existing) {
      return existing;
    }
    const now = Date.now();
    const board: ForumBoard = {
      id: boardId,
      orgId,
      name: '综合讨论',
      intro: '默认板块：开发讨论与方案沉淀的起点',
      sort: 0,
      archived: false,
      createdBy: rootId,
      createdAt: now,
      updatedAt: now
    };
    await this.sdk.docs.put(FORUM_COLLECTIONS.boards, board.id, board as unknown as Record<string, unknown>);
    return board;
  }

  /**
   * 默认板块播种完成后回写标记（lww 单文档）。标记只是省去每次加载时的
   * 一次 docs.get；幂等性由 seedDefaultBoard 的固定板块 id 保证。
   */
  async markBoardsSeeded(config: ForumOrgConfig): Promise<ForumOrgConfig> {
    await this.ensureCollectionsDeclared();
    const updated: ForumOrgConfig = { ...config, boardsSeeded: true };
    await this.sdk.docs.put(FORUM_COLLECTIONS.orgConfig, updated.orgId, updated as unknown as Record<string, unknown>);
    return updated;
  }

  /** 板块管理（档三-11：名册管理员） */
  async createBoard(
    orgId: string,
    rootId: string,
    input: { name: string; intro: string; sort?: number },
    role: ForumRole
  ): Promise<ForumBoard> {
    if (!canManageBoards(role)) {
      throw new Error('仅组织管理员可以创建板块');
    }
    // 服务层纵深校验（视图层单点把关可被绕过；板块名长度同时是摘要 200 字符约束的前置）
    const validation = validateBoardInput(input.name, input.intro);
    if (!validation.ok) {
      throw new Error(validation.reason);
    }
    await this.ensureCollectionsDeclared();
    const now = Date.now();
    const board: ForumBoard = {
      id: newId('board'),
      orgId,
      name: normalizeForumText(input.name),
      intro: normalizeForumText(input.intro),
      sort: input.sort ?? 100,
      archived: false,
      createdBy: rootId,
      createdAt: now,
      updatedAt: now
    };
    await this.sdk.docs.put(FORUM_COLLECTIONS.boards, board.id, board as unknown as Record<string, unknown>);
    return board;
  }

  async updateBoard(
    board: ForumBoard,
    patch: { name?: string; intro?: string; sort?: number },
    role: ForumRole
  ): Promise<ForumBoard> {
    if (!canManageBoards(role)) {
      throw new Error('仅组织管理员可以修改板块');
    }
    // 服务层纵深校验（同 createBoard）
    const validation = validateBoardInput(patch.name ?? board.name, patch.intro ?? board.intro);
    if (!validation.ok) {
      throw new Error(validation.reason);
    }
    await this.ensureCollectionsDeclared();
    const updated: ForumBoard = {
      ...board,
      name: patch.name !== undefined ? normalizeForumText(patch.name) : board.name,
      intro: patch.intro !== undefined ? normalizeForumText(patch.intro) : board.intro,
      sort: patch.sort ?? board.sort,
      updatedAt: Date.now()
    };
    await this.sdk.docs.put(FORUM_COLLECTIONS.boards, updated.id, updated as unknown as Record<string, unknown>);
    return updated;
  }

  async archiveBoard(board: ForumBoard, archived: boolean, role: ForumRole): Promise<ForumBoard> {
    if (!canManageBoards(role)) {
      throw new Error('仅组织管理员可以归档板块');
    }
    await this.ensureCollectionsDeclared();
    const updated: ForumBoard = { ...board, archived, updatedAt: Date.now() };
    await this.sdk.docs.put(FORUM_COLLECTIONS.boards, updated.id, updated as unknown as Record<string, unknown>);
    return updated;
  }

  /**
   * 域身份签名（identity:sign）。载荷编入作者身份四元组，验签侧按同一函数
   * 从记录当前字段重算比对。该权限是「使用时询问」高危权限，用户拒绝时桥
   * 会抛错——降级为不签名（记录无「已签名」徽标），不阻断主流程。
   */
  private async signRecord(orgId: string, recordId: string, authorRootId: string, content: string): Promise<ForumSignature | null> {
    const payload = buildForumSignPayload(orgId, recordId, authorRootId, content);
    try {
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-forum] 签名被拒或不可用，记录将不带签名徽标：', error);
      return null;
    }
  }

  /**
   * 验签（identity.verify 免权限）：正确姿势是「重算后比对」而非「回放随帖
   * payload」——先从记录当前字段重算期望载荷，与随帖 payload 不等即判 false
   * （内容/作者/归属任一被改都会失配），相等才交给密码学验签。
   * 剩余缺口（沿用 spark-example 诚实标注）：未校验 publicKey 与
   * authorRootId 的绑定（需域身份目录，超出 MVP 范围）。
   */
  private async verifySignature(
    orgId: string,
    recordId: string,
    authorRootId: string,
    content: string,
    signature: ForumSignature | undefined
  ): Promise<boolean> {
    if (!signature) {
      return false;
    }
    const expected = buildForumSignPayload(orgId, recordId, authorRootId, content);
    if (signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, signature.signature, signature.publicKey);
    return result.valid;
  }

  async verifyTopicSignature(topic: ForumTopic): Promise<boolean> {
    return this.verifySignature(topic.orgId, topic.id, topic.authorRootId, `${topic.title}\n${topic.content}`, topic.signature);
  }

  async verifyReplySignature(reply: ForumReply): Promise<boolean> {
    return this.verifySignature(reply.orgId, reply.id, reply.authorRootId, reply.content, reply.signature);
  }

  async verifyTopicEventSignature(event: ForumTopicEvent): Promise<boolean> {
    return this.verifySignature(event.orgId, event.id, event.operatorRootId, `${event.kind}:${event.reason ?? ''}`, event.signature);
  }

  /**
   * 发主题（全员可发，§5 权限反转）。编辑语义：传 supersedes 即「编辑=新版本」，
   * 仅作者本人可对自己的主题发新版本；签名载荷绑定标题+正文。
   * 已归档板块拦截新主题（编辑历史版本不受限——归档只冻结新增）。
   */
  async createTopic(
    orgId: string,
    rootId: string,
    input: { boardId: string; title: string; content: string; tags?: string[]; supersedes?: ForumTopic },
    role: ForumRole
  ): Promise<ForumTopic> {
    if (!canPostTopic(role)) {
      throw new Error('仅组织成员可以发布主题');
    }
    if (input.supersedes && input.supersedes.authorRootId !== rootId) {
      throw new Error('只能编辑自己发布的主题');
    }
    await this.ensureCollectionsDeclared();
    const board = await this.sdk.docs.get<ForumBoard>(FORUM_COLLECTIONS.boards, input.boardId);
    if (!board) {
      throw new Error('板块不存在或尚未同步到本机');
    }
    if (board.archived && !input.supersedes) {
      throw new Error('板块已归档，不能发布新主题');
    }
    const title = normalizeForumText(input.title);
    const content = normalizeForumText(input.content);
    const topic: ForumTopic = {
      id: newId('topic'),
      orgId,
      boardId: input.boardId,
      title,
      content,
      contentFormat: 'plain',
      tags: input.tags,
      authorRootId: rootId,
      createdAt: Date.now(),
      supersedesId: input.supersedes?.id
    };

    const signature = await this.signRecord(orgId, topic.id, rootId, `${title}\n${content}`);
    if (signature) {
      topic.signature = signature;
    }

    await this.sdk.docs.put(FORUM_COLLECTIONS.topics, topic.id, topic as unknown as Record<string, unknown>);
    return topic;
  }

  /**
   * 回复（全员可回）。已关闭/已隐藏主题拦截：关闭状态是事件流派生量，
   * 派生时按名册管理员集合鉴权（与读侧 deriveTopicState 同口径）。
   */
  async createReply(
    orgId: string,
    rootId: string,
    input: { topicId: string; content: string; parentReplyId?: string },
    role: ForumRole,
    adminRootIds: ReadonlySet<string>
  ): Promise<ForumReply> {
    if (!canReplyTopic(role)) {
      throw new Error('仅组织成员可以回复');
    }
    await this.ensureCollectionsDeclared();
    const events = await this.loadTopicEvents(orgId);
    const topicState = deriveTopicState(input.topicId, events, adminRootIds);
    if (topicState.closed) {
      throw new Error('主题已关闭，不能回复');
    }
    if (topicState.hidden) {
      throw new Error('主题已隐藏，不能回复');
    }
    const content = normalizeForumText(input.content);
    const reply: ForumReply = {
      id: newId('reply'),
      orgId,
      topicId: input.topicId,
      parentReplyId: input.parentReplyId,
      content,
      authorRootId: rootId,
      createdAt: Date.now()
    };

    const signature = await this.signRecord(orgId, reply.id, rootId, content);
    if (signature) {
      reply.signature = signature;
    }

    await this.sdk.docs.put(FORUM_COLLECTIONS.replies, reply.id, reply as unknown as Record<string, unknown>);
    return reply;
  }

  /**
   * 主题治理事件（置顶/加精/已解决/关闭/隐藏等，档三-11：名册管理员）。
   * 治理动作全部留痕可审计、操作者可签名（append-only 事件流，档三-10）。
   * 删除诉求按档三-14 以「关闭 + 隐藏」事件表达——没有真删除，UI 提交时
   * 如实告知不可收回。
   */
  async createTopicEvent(
    orgId: string,
    rootId: string,
    input: { topicId: string; kind: ForumTopicEventKind; reason?: string },
    role: ForumRole
  ): Promise<ForumTopicEvent> {
    if (!canModerateTopics(role)) {
      throw new Error('仅组织管理员可以执行主题治理操作');
    }
    await this.ensureCollectionsDeclared();
    const event: ForumTopicEvent = {
      id: newId('event'),
      orgId,
      topicId: input.topicId,
      kind: input.kind,
      operatorRootId: rootId,
      reason: input.reason ? normalizeForumText(input.reason) : undefined,
      createdAt: Date.now()
    };

    const signature = await this.signRecord(orgId, event.id, rootId, `${event.kind}:${event.reason ?? ''}`);
    if (signature) {
      event.signature = signature;
    }

    await this.sdk.docs.put(
      FORUM_COLLECTIONS.topicEvents,
      event.id,
      event as unknown as Record<string, unknown>
    );
    return event;
  }

  /**
   * 发主题后向组织应用会话发通知（发帖者路径，仅发帖者本机即时反馈；
   * 档三-12：回复不通知、仅新主题通知）。卡片 data 只放引用 {topicId, orgId}。
   * 降级：权限被拒/内核限流（10 条/60s）时不阻断发帖，返回 false。
   */
  async notifyNewTopic(topic: ForumTopic, boardName: string): Promise<boolean> {
    if (!this.sdk.messages) {
      // tab 同进程模式无 messages 模块（SDK 契约上为可选字段）
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: buildTopicSummary(boardName, topic.title), topicId: topic.id, orgId: topic.orgId },
        { viewId: 'topic-card', data: { topicId: topic.id, orgId: topic.orgId } }
      );
      // 记入已通知台账（sdk.data 持久面为准）：成员侧本地生成路径（notifyNewTopics）不会补发重复通知
      await this.markNotified(topic.orgId, topic);
      return true;
    } catch (error) {
      console.warn('[spark-forum] 应用消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /**
   * 成员侧「本地生成」通知（服务号模型 §20.4.3）：应用消息不走网络，发帖者
   * 的通知只到发帖者本机；主题数据经 org 同步到达每台成员设备后，各设备上
   * 的插件实例从本机数据各自算出通知写入本机会话。去重靠 sdk.data 持久台账
   * （`spark-forum:notified` scope:'local'，去重键 = 主题 id），重启/iframe
   * 重建后不重复；遇内核限流即中止本轮，未记账的留待下次加载补齐。
   *
   * 只对新主题生成通知（档三-12）；编辑新版本（supersedesId 非空）不重复通知。
   */
  async notifyNewTopics(orgId: string, topics: ForumTopic[], boardNameByBoardId: Map<string, string>): Promise<number> {
    if (!this.sdk.messages || topics.length === 0) {
      return 0;
    }
    const notified = await this.loadNotifiedTopicIds(orgId);
    let sent = 0;
    for (const topic of topics) {
      // 编辑产生的新版本不是「新主题」，不通知
      if (topic.supersedesId || notified.has(topic.id)) {
        continue;
      }
      const ok = await this.notifyNewTopic(topic, boardNameByBoardId.get(topic.boardId) ?? '');
      if (!ok) {
        // 限流/权限降级：本轮放弃，未记账的主题下次加载时再补
        break;
      }
      notified.add(topic.id);
      sent += 1;
    }
    return sent;
  }

  async loadBoards(orgId: string): Promise<ForumBoard[]> {
    const response = await this.sdk.docs.query<ForumBoard>(FORUM_COLLECTIONS.boards, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: false,
      limit: FORUM_QUERY_LIMITS.boards
    });

    return response.items
      .map((item) => item.data)
      .sort((a, b) => a.sort - b.sort || a.createdAt - b.createdAt);
  }

  async loadTopics(orgId: string): Promise<ForumTopic[]> {
    const response = await this.sdk.docs.query<ForumTopic>(FORUM_COLLECTIONS.topics, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: true,
      limit: FORUM_QUERY_LIMITS.topics
    });

    return response.items.map((item) => item.data).sort((a, b) => b.createdAt - a.createdAt);
  }

  async loadReplies(orgId: string): Promise<ForumReply[]> {
    const response = await this.sdk.docs.query<ForumReply>(FORUM_COLLECTIONS.replies, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: false,
      limit: FORUM_QUERY_LIMITS.replies
    });

    return response.items.map((item) => item.data).sort((a, b) => a.createdAt - b.createdAt);
  }

  async loadTopicEvents(orgId: string): Promise<ForumTopicEvent[]> {
    const response = await this.sdk.docs.query<ForumTopicEvent>(FORUM_COLLECTIONS.topicEvents, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: false,
      limit: FORUM_QUERY_LIMITS.topicEvents
    });

    return response.items.map((item) => item.data).sort((a, b) => a.createdAt - b.createdAt);
  }
}
