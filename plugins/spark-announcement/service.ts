/**
 * 公告通知插件（spark-announcement）· 业务服务层。
 *
 * 架构纪律（沿用 spark-forum「服务层集中 SDK 调用」）：所有 SDK 调用集中在
 * 服务层，视图组件不直接碰 sdk.docs——便于单测（tests/ 用 mock SDK 驱动本层）
 * 与权限审计（本文件即插件能力面清单）。
 *
 * 本层协作的 SDK 能力：
 *   1) docs（storage:read/write）：集合声明 + 文档读写，同步策略写入前必须声明；
 *   2) identity（identity:sign）：公告/撤回防抵赖签名，验签免权限；
 *   3) messages（message:app）：公告应用会话卡片——发布者路径只是发布者本机的
 *      即时反馈；成员侧按服务号模型（p2p-messages §20.4.3）在同步后从本机数据
 *      「本地生成」卡片（见 notifyNewAnnouncements），同步的是数据不是消息。
 *
 * 权限降级原则：签名与应用消息是「增强能力」，授权被拒或限流时不阻断主流程
 * （公告照发），仅少一个徽标/少一条卡片。
 *
 * 拍板口径落点：
 * - 档一-2：MVP 只做手动公告（无契约接入、无自动起草），version/releaseRef
 *   为手动自由字段；
 * - 档二-4：后台视图常驻监听排后续迭代，MVP 用「插件加载时补发 + 节流」；
 * - 档三-23：发布权集合由名册管理员直改初始化（saveConfig）；
 * - 撤回 = 追加撤回记录，不回滚已送达卡片、不删原记录。
 */
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  announcementSignContent,
  buildAnnouncementSignPayload,
  buildAnnouncementSummary,
  buildHistorySummary,
  canManageAnnounceConfig,
  canPublishAnnouncement,
  canRetractAnnouncement,
  deriveRetractionMap,
  findApplicableRetraction,
  normalizeAnnouncementText,
  retractionSignContent,
  selectBackfillBatch,
  validateAnnouncementBody,
  validateAnnouncementTitle,
  validateRetractReason,
  validateVersionFields,
  type Announcement,
  type AnnouncementConfig,
  type AnnouncementKind,
  type AnnouncementRetraction,
  type AnnouncementSignature
} from './model';

// 配置类型定义在 model 层（纯数据结构），此处再导出便于视图层从 service 单一入口引入
export type { AnnouncementConfig } from './model';

export const ANNOUNCEMENT_COLLECTIONS = {
  config: 'announcement_config',
  items: 'announcement_items',
  retractions: 'announcement_retractions'
} as const;

/**
 * 集合同步策略声明（写入前必须声明，启动时统一声明一次）：
 * - config：发布权集合与类型开关，可被后续管理员调整覆盖，显式 lww；
 * - items / retractions：公告本体与撤回留痕，仅追加、不覆盖、不删除，
 *   append-only（自动链式存证）——公告不可编辑，撤回不改原记录。
 */
const ANNOUNCEMENT_COLLECTION_SCHEMAS = {
  [ANNOUNCEMENT_COLLECTIONS.config]: { syncStrategy: 'lww' },
  [ANNOUNCEMENT_COLLECTIONS.items]: { syncStrategy: 'append-only' },
  [ANNOUNCEMENT_COLLECTIONS.retractions]: { syncStrategy: 'append-only' }
} as const;

type OrgRole = 'admin' | 'member' | null | undefined;

function newId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

/**
 * 「已送达公告」幂等去重台账（设计稿 §3 送达状态集合，scope=local）：按空间
 * （orgId）记录本机已为哪些公告 id 生成过应用消息（去重键 = 公告 id，值携带
 * publishedAt 水位），防重启后重复推卡片。应用消息是「本地生成、本地消费」
 * （§20.4.3）——消息本身不同步，送达状态因此也只须是本机状态。
 *
 * 持久面选型（H1 评审修复）：插件沙箱 iframe 是 opaque origin（壳层
 * sandbox="allow-scripts"），localStorage 恒抛 SecurityError，且 iframe 随
 * 标签切换销毁重建、进程内兜底一并清零——只用 localStorage 会导致每次打开
 * 插件为全部历史公告重发卡片。故台账迁到 sdk.data 持久面：
 * `spark-announcement:delivery`（declareCollection scope:'local'，键
 * `{orgId}:{announcementId}`）；localStorage 仅作缓存/兜底（持久面读写失败
 * 时降级），读侧以持久台账为准。
 */
const DELIVERY_COLLECTION = 'spark-announcement:delivery';
const DELIVERED_KEY_PREFIX = 'spark-announcement:delivered:';
const memoryDeliveredFallback = new Map<string, Set<string>>();

function deliveredStorageKey(orgId: string): string {
  return `${DELIVERED_KEY_PREFIX}${orgId}`;
}

/** localStorage 缓存读（含进程内兜底；仅作持久面不可用时的降级来源） */
function loadDeliveredCache(orgId: string): Set<string> {
  const key = deliveredStorageKey(orgId);
  try {
    const raw = globalThis.localStorage?.getItem(key);
    if (raw) {
      return new Set(JSON.parse(raw) as string[]);
    }
  } catch {
    /* opaque origin 沙箱恒抛 SecurityError、隐私模式或数据损坏：走进程内兜底 */
  }
  return new Set(memoryDeliveredFallback.get(key) ?? []);
}

/** localStorage 缓存写（best-effort；进程内兜底先行，存储不可用不阻塞） */
function saveDeliveredCache(orgId: string, ids: Set<string>): void {
  const key = deliveredStorageKey(orgId);
  memoryDeliveredFallback.set(key, new Set(ids));
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify([...ids]));
  } catch {
    /* 存储不可用时进程内兜底已记录，忽略 */
  }
}

/**
 * 送达熔断观测面（设计稿 §3/§4：超限计入 rejectedCount）。进程内计数，
 * 视图层可展示；rate-limited/权限拒绝都计入，不重试轰炸。
 */
export type AnnouncementDeliveryStats = {
  sentCount: number;
  rejectedCount: number;
};

export class AnnouncementService {
  private collectionsReady: Promise<void> | null = null;
  private readonly stats: AnnouncementDeliveryStats = { sentCount: 0, rejectedCount: 0 };
  private deliveryCollectionReady: Promise<void> | null = null;
  /**
   * 送达链路进程内 mutex（S3）：按空间串行化「发布即时通知」与「加载时补发」，
   * 防止并发批次（如主视图 reload 与同步回调同时触发）对同一条公告重复发卡片。
   */
  private readonly deliveryChains = new Map<string, Promise<unknown>>();

  constructor(private readonly sdk: PluginSDK) {}

  /** 声明本插件全部集合的同步策略（幂等，重复声明与首次一致即可） */
  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(ANNOUNCEMENT_COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  /** 声明送达台账集合（sdk.data scope:'local'，幂等；代际内策略冲突报错） */
  private ensureDeliveryCollection(): Promise<void> {
    this.deliveryCollectionReady ??= this.sdk.data
      .declareCollection({ name: DELIVERY_COLLECTION, scope: 'local' })
      .then(() => undefined);
    return this.deliveryCollectionReady;
  }

  /** 同一空间的送达操作串行化（前序失败不阻塞后续任务） */
  private enqueueDelivery<T>(orgId: string, task: () => Promise<T>): Promise<T> {
    const prev = this.deliveryChains.get(orgId) ?? Promise.resolve();
    const next = prev.then(task, task);
    this.deliveryChains.set(orgId, next);
    return next;
  }

  /**
   * 读送达台账：以 sdk.data 持久面为准；持久面不可用（声明/查询失败）时降级
   * localStorage 缓存 + 进程内兜底。读成功后回写缓存，供降级路径使用。
   */
  private async loadDeliveredIds(orgId: string): Promise<Set<string>> {
    try {
      await this.ensureDeliveryCollection();
      const response = await this.sdk.data.query<{ publishedAt?: number }>(DELIVERY_COLLECTION, {
        prefix: `${orgId}:`,
        limit: 2000
      });
      const ids = new Set(response.items.map((item) => item.key.slice(orgId.length + 1)));
      saveDeliveredCache(orgId, ids);
      return ids;
    } catch (error) {
      console.warn('[spark-announcement] 送达台账持久面读取失败，降级缓存（本次会话内去重）：', error);
      return loadDeliveredCache(orgId);
    }
  }

  /** 记送达台账：持久面为主，缓存兜底同步刷新（持久面写失败不阻塞送达流程） */
  private async markDelivered(orgId: string, announcementId: string, publishedAt: number): Promise<void> {
    const cached = loadDeliveredCache(orgId);
    cached.add(announcementId);
    saveDeliveredCache(orgId, cached);
    try {
      await this.ensureDeliveryCollection();
      await this.sdk.data.save(DELIVERY_COLLECTION, `${orgId}:${announcementId}`, { publishedAt });
    } catch (error) {
      console.warn('[spark-announcement] 送达台账持久面写入失败（缓存已记，降级为会话内去重）：', error);
    }
  }

  /** 送达熔断观测面（rejectedCount 等，进程内累计） */
  getDeliveryStats(): AnnouncementDeliveryStats {
    return { ...this.stats };
  }

  // ------------------------------------------------------------------
  // 发布权配置（档三-23：MVP 名册管理员直改初始化）
  // ------------------------------------------------------------------

  async loadConfig(orgId: string): Promise<AnnouncementConfig | null> {
    await this.ensureCollectionsDeclared();
    return this.sdk.docs.get<AnnouncementConfig>(ANNOUNCEMENT_COLLECTIONS.config, orgId);
  }

  /**
   * 初始化/直改发布权配置（lww 单文档，文档 id = orgId）。仅名册管理员；
   * 发布权集合的演化挂组织治理事务排「规则挂事务」迭代（档三-23）。
   */
  async saveConfig(
    orgId: string,
    rootId: string,
    input: { publisherRootIds: string[]; enableRelease?: boolean; enableNotice?: boolean },
    role: OrgRole
  ): Promise<AnnouncementConfig> {
    if (!canManageAnnounceConfig(role)) {
      throw new Error('仅组织管理员可以维护发布权配置');
    }
    await this.ensureCollectionsDeclared();
    const existing = await this.loadConfig(orgId);
    const publisherRootIds = [...new Set(input.publisherRootIds.map((item) => item.trim()).filter(Boolean))];
    const config: AnnouncementConfig = {
      orgId,
      publisherRootIds,
      enableRelease: input.enableRelease ?? true,
      enableNotice: input.enableNotice ?? true,
      createdBy: existing?.createdBy ?? rootId,
      createdAt: existing?.createdAt ?? Date.now(),
      updatedAt: Date.now()
    };
    await this.sdk.docs.put(
      ANNOUNCEMENT_COLLECTIONS.config,
      orgId,
      config as unknown as Record<string, unknown>
    );
    return config;
  }

  // ------------------------------------------------------------------
  // 签名（identity:sign 防抵赖；验签免权限）
  // ------------------------------------------------------------------

  /**
   * 域身份签名。载荷编入发布者身份四元组，验签侧按同一函数从记录当前字段
   * 重算比对。identity:sign 是「使用时询问」高危权限，用户拒绝时桥会抛错——
   * 降级为不签名（记录无「已签名」徽标），不阻断主流程。
   */
  private async signRecord(
    orgId: string,
    recordId: string,
    rootId: string,
    content: string
  ): Promise<AnnouncementSignature | null> {
    const payload = buildAnnouncementSignPayload(orgId, recordId, rootId, content);
    try {
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-announcement] 签名被拒或不可用，记录将不带签名徽标：', error);
      return null;
    }
  }

  /**
   * 验签（identity.verify 免权限）：正确姿势是「重算后比对」而非「回放随记录
   * payload」——先从记录当前字段重算期望载荷，与随记录 payload 不等即判 false，
   * 相等才交给密码学验签。剩余缺口（沿用 spark-forum 诚实标注）：未校验
   * publicKey 与 publisherRootId 的绑定（需域身份目录，超出 MVP 范围）。
   */
  private async verifySignature(
    orgId: string,
    recordId: string,
    rootId: string,
    content: string,
    signature: AnnouncementSignature | undefined
  ): Promise<boolean> {
    if (!signature) {
      return false;
    }
    const expected = buildAnnouncementSignPayload(orgId, recordId, rootId, content);
    if (signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, signature.signature, signature.publicKey);
    return result.valid;
  }

  async verifyAnnouncementSignature(announcement: Announcement): Promise<boolean> {
    return this.verifySignature(
      announcement.orgId,
      announcement.id,
      announcement.publisherRootId,
      announcementSignContent(announcement),
      announcement.signature
    );
  }

  async verifyRetractionSignature(retraction: AnnouncementRetraction): Promise<boolean> {
    return this.verifySignature(
      retraction.orgId,
      retraction.id,
      retraction.retractorRootId,
      retractionSignContent(retraction.targetAnnouncementId, retraction.reason),
      retraction.signature
    );
  }

  // ------------------------------------------------------------------
  // 发布与撤回
  // ------------------------------------------------------------------

  /**
   * 发布公告（发布权集合成员，业务层校验；配置未初始化时 fail-closed）。
   * 类型开关（enableRelease/enableNotice）在配置层面收口。公告 append-only
   * 不可编辑——纠错 = 发更正公告或撤回。
   * 长度/形态校验在服务层入口 fail-closed（U2：不能只靠视图层把关，自制
   * 客户端或后续契约调用方都会绕过视图）。
   */
  async publishAnnouncement(
    orgId: string,
    rootId: string,
    input: {
      kind: AnnouncementKind;
      title: string;
      body: string;
      version?: string;
      releaseRef?: string;
    },
    config: AnnouncementConfig | null
  ): Promise<Announcement> {
    if (!canPublishAnnouncement(config, rootId)) {
      throw new Error('仅发布权集合成员可以发布公告（请联系组织管理员初始化/维护发布权配置）');
    }
    if (input.kind === 'release' && config?.enableRelease === false) {
      throw new Error('本组织已关闭版本公告类型');
    }
    if (input.kind === 'notice' && config?.enableNotice === false) {
      throw new Error('本组织已关闭团队通知类型');
    }
    const titleCheck = validateAnnouncementTitle(input.title);
    if (!titleCheck.ok) {
      throw new Error(titleCheck.reason ?? '标题不合法');
    }
    const bodyCheck = validateAnnouncementBody(input.body);
    if (!bodyCheck.ok) {
      throw new Error(bodyCheck.reason ?? '正文不合法');
    }
    const versionCheck = validateVersionFields(input.version, input.releaseRef);
    if (!versionCheck.ok) {
      throw new Error(versionCheck.reason ?? '版本字段不合法');
    }
    await this.ensureCollectionsDeclared();
    const title = normalizeAnnouncementText(input.title);
    const body = normalizeAnnouncementText(input.body);
    const version = input.version ? normalizeAnnouncementText(input.version) : undefined;
    const releaseRef = input.releaseRef ? normalizeAnnouncementText(input.releaseRef) : undefined;
    const announcement: Announcement = {
      id: newId('ann'),
      orgId,
      kind: input.kind,
      title,
      body,
      version: input.kind === 'release' ? version : undefined,
      releaseRef: input.kind === 'release' ? releaseRef : undefined,
      publisherRootId: rootId,
      publishedAt: Date.now()
    };

    const signature = await this.signRecord(
      orgId,
      announcement.id,
      rootId,
      announcementSignContent(announcement)
    );
    if (signature) {
      announcement.signature = signature;
    }

    await this.sdk.docs.put(
      ANNOUNCEMENT_COLLECTIONS.items,
      announcement.id,
      announcement as unknown as Record<string, unknown>
    );
    return announcement;
  }

  /**
   * 撤回公告（发布权集合成员或名册管理员）：追加一条撤回记录，不改不删原
   * 公告（append-only 不可删改，已扩散内容不可收回）。重复撤回同一公告视为
   * 更新撤回理由（派生层保留最新一条，历史留痕均可查）。
   */
  async retractAnnouncement(
    orgId: string,
    rootId: string,
    input: { targetAnnouncementId: string; reason?: string },
    config: AnnouncementConfig | null,
    role: OrgRole
  ): Promise<AnnouncementRetraction> {
    if (!canRetractAnnouncement(config, rootId, role)) {
      throw new Error('仅发布权集合成员或组织管理员可以撤回公告');
    }
    const reasonCheck = validateRetractReason(input.reason);
    if (!reasonCheck.ok) {
      throw new Error(reasonCheck.reason ?? '撤回理由不合法');
    }
    await this.ensureCollectionsDeclared();
    const target = await this.sdk.docs.get<Announcement>(ANNOUNCEMENT_COLLECTIONS.items, input.targetAnnouncementId);
    if (!target || target.orgId !== orgId) {
      throw new Error('目标公告不存在或尚未同步到本机');
    }
    const retraction: AnnouncementRetraction = {
      id: newId('retract'),
      orgId,
      targetAnnouncementId: input.targetAnnouncementId,
      reason: input.reason ? normalizeAnnouncementText(input.reason) : undefined,
      retractorRootId: rootId,
      retractedAt: Date.now()
    };

    const signature = await this.signRecord(
      orgId,
      retraction.id,
      rootId,
      retractionSignContent(retraction.targetAnnouncementId, retraction.reason)
    );
    if (signature) {
      retraction.signature = signature;
    }

    await this.sdk.docs.put(
      ANNOUNCEMENT_COLLECTIONS.retractions,
      retraction.id,
      retraction as unknown as Record<string, unknown>
    );
    return retraction;
  }

  // ------------------------------------------------------------------
  // 应用会话卡片送达（message:app；本地生成、本地消费，§20.4）
  // ------------------------------------------------------------------

  /**
   * 为一条公告生成本机应用会话卡片（summary 强制 + announce-card 卡片，
   * data 只携带引用 {announcementId, orgId}——正文经 docs 查询，不随消息
   * 冗余落库）。发布者路径只是发布者本机的即时反馈；成员侧走
   * notifyNewAnnouncements 本地生成。
   * 降级：权限被拒/内核限流（10 条/60s）时不阻断，计入 rejectedCount，返回 false。
   * 经进程内 mutex 按空间串行化（S3），与补发批次互不穿插。
   */
  async notifyAnnouncement(announcement: Announcement): Promise<boolean> {
    return this.enqueueDelivery(announcement.orgId, () => this.sendAnnouncementCard(announcement));
  }

  /** 发送单条公告卡片并入台账（内部实现，调用方须已持有本空间的送达串行链） */
  private async sendAnnouncementCard(announcement: Announcement): Promise<boolean> {
    if (!this.sdk.messages) {
      // tab 同进程模式无 messages 模块（SDK 契约上为可选字段）
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        {
          summary: buildAnnouncementSummary(announcement),
          announcementId: announcement.id,
          orgId: announcement.orgId
        },
        { viewId: 'announce-card', data: { announcementId: announcement.id, orgId: announcement.orgId } }
      );
      // 记入已送达台账（sdk.data 持久面为准）：成员侧本地生成路径不会补发重复卡片
      await this.markDelivered(announcement.orgId, announcement.id, announcement.publishedAt);
      this.stats.sentCount += 1;
      return true;
    } catch (error) {
      this.stats.rejectedCount += 1;
      console.warn('[spark-announcement] 应用消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /**
   * 成员侧「本地生成」卡片（服务号模型 §20.4.3）：应用消息不走网络，公告数据
   * 经 org 同步到达每台成员设备后，各设备上的插件实例从本机数据各自算出卡片
   * 写入本机应用会话（app:spark-announcement）。插件加载时补发（档二-4 MVP
   * 降级口径），幂等去重靠 sdk.data 持久台账（`spark-announcement:delivery`
   * scope:'local'，去重键 = 公告 id），重启/iframe 重建后不重复。
   *
   * 节流（设计稿 §3 限流预算）：
   * - 待补发超过阈值（ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD）时只补最新一条卡片
   *   + 一条「另有 N 条历史公告」汇总消息，其余直接记账，防刷屏；
   * - 遇内核限流/权限拒绝即中止本轮（不重试轰炸），未记账的留待下次加载补齐，
   *   拒绝计入 rejectedCount 熔断观测面。
   *
   * 已撤回的公告不再生成新卡片（数据仍在列表可查）；已送达的卡片由卡片视图
   * 实时查询撤回状态标注「已撤回」。经进程内 mutex 按空间串行化（S3）。
   */
  async notifyNewAnnouncements(
    orgId: string,
    announcements: Announcement[],
    retractions: AnnouncementRetraction[],
    retractorRootIds: ReadonlySet<string>
  ): Promise<number> {
    return this.enqueueDelivery(orgId, async () => {
      if (!this.sdk.messages || announcements.length === 0) {
        return 0;
      }
      const retractionMap = deriveRetractionMap(retractions, retractorRootIds);
      const delivered = await this.loadDeliveredIds(orgId);
      const pending = announcements
        .filter((item) => !delivered.has(item.id) && !findApplicableRetraction(item, retractionMap))
        .sort((a, b) => a.publishedAt - b.publishedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      if (pending.length === 0) {
        return 0;
      }

      const batch = selectBackfillBatch(pending);
      let sent = 0;

      // 历史汇总先行：汇总消息失败则本轮整体中止（什么都不记账），下次加载重试
      if (batch.summarizedCount > 0) {
        const summaryOk = await this.sendHistorySummary(orgId, batch.summarizedCount);
        if (!summaryOk) {
          return 0;
        }
        for (const item of batch.summarized) {
          await this.markDelivered(orgId, item.id, item.publishedAt);
        }
      }

      for (const announcement of batch.cards) {
        // 直调内部实现（本批次已在送达串行链上，不能再 enqueue 否则自锁）
        const ok = await this.sendAnnouncementCard(announcement);
        if (!ok) {
          // 限流/权限降级：本轮放弃，未记账的公告下次加载时再补
          break;
        }
        sent += 1;
      }
      return sent;
    });
  }

  /** 「另有 N 条历史公告」汇总消息（节流补发防刷屏，设计稿 §3） */
  private async sendHistorySummary(orgId: string, count: number): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage({ summary: buildHistorySummary(count), orgId });
      this.stats.sentCount += 1;
      return true;
    } catch (error) {
      this.stats.rejectedCount += 1;
      console.warn('[spark-announcement] 历史公告汇总消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  // ------------------------------------------------------------------
  // 查询（orgId 一级过滤，跨设备同步口径稳定）
  // ------------------------------------------------------------------

  async loadAnnouncements(orgId: string): Promise<Announcement[]> {
    const response = await this.sdk.docs.query<Announcement>(ANNOUNCEMENT_COLLECTIONS.items, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: true,
      limit: 1000
    });

    return response.items
      .map((item) => item.data)
      .sort((a, b) => b.publishedAt - a.publishedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  async loadRetractions(orgId: string): Promise<AnnouncementRetraction[]> {
    const response = await this.sdk.docs.query<AnnouncementRetraction>(ANNOUNCEMENT_COLLECTIONS.retractions, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: false,
      limit: 1000
    });

    return response.items
      .map((item) => item.data)
      .sort((a, b) => a.retractedAt - b.retractedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }
}
