/**
 * 公告通知插件（spark-announcement）· 数据模型与纯函数。
 *
 * 设计依据：wiki/product/bootstrap-plugins/announcement.md v0.2 §3（数据模型）。
 *
 * 核心模型（设计稿 §1）：公告本体是组织空间的同步数据（append-only 集合经
 * orgsync 同步），消息卡片是各成员本机插件实例从同步数据「本地算出」的送达
 * 形态（p2p-messages §20.4：同步的是数据，不是消息）。本文件只承载数据结构
 * 与确定性纯函数，SDK 调用全部集中在 service.ts。
 *
 * 拍板口径落点：
 * - 档一-2：MVP 只做手动公告；version/releaseRef 为手动自由字段（展示级，
 *   档三-24：releaseRef = 发布单集合记录 id 字符串），契约落地后收敛；
 * - 档三-23：发布权集合由组织管理员直改初始化（announcement_config lww）；
 * - 撤回 = 追加撤回记录（append-only），不回滚已送达卡片，不伪造「从未发布」；
 * - 档三-9 同口径：MVP 正文纯文本，超长正文走内容面 blob 排后续迭代。
 *
 * 文档结构演进纪律（沿用 spark-forum 头注）：只加可选字段、不改不删既有字段。
 */

/** 公告标题上限 */
export const ANNOUNCEMENT_MAX_TITLE_LENGTH = 120;
/** 公告正文上限（MVP 纯文本；超长正文走 sdk.content blob 排后续迭代） */
export const ANNOUNCEMENT_MAX_BODY_LENGTH = 20000;
/** 版本号上限（自由字段，展示级） */
export const ANNOUNCEMENT_MAX_VERSION_LENGTH = 40;
/** 发布记录引用上限（档三-24：发布单集合记录 id，自由字段） */
export const ANNOUNCEMENT_MAX_RELEASE_REF_LENGTH = 120;
/** 撤回理由上限 */
export const ANNOUNCEMENT_MAX_RETRACT_REASON_LENGTH = 200;

/** 应用消息 summary 硬上限（内核 APP_SUMMARY_MAX_CHARS，超限内核拒绝写入） */
export const ANNOUNCEMENT_SUMMARY_LIMIT = 200;

/**
 * 历史公告补发阈值（设计稿 §3 限流预算）：一次性同步到的未送达公告超过
 * 该数时，只补最新一条卡片 + 一条「另有 N 条历史公告」汇总消息，防刷屏；
 * 内核限流为每（空间, 插件）60 秒 10 条（APP_MSG_RATE_LIMIT），本阈值远低于
 * 配额，补发节奏遇 rate-limited 即中止、下次加载再续。
 */
export const ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD = 5;

/** 公告类型（设计稿 §2：版本公告 | 团队通知） */
export type AnnouncementKind = 'release' | 'notice';

/**
 * 签名信息（identity:sign 防抵赖）。随记录存储：签名出自插件域身份
 * （域私钥永不离开内核），任何成员拿到 payload + signature + publicKey
 * 都可用 identity.verify 免权限验签。与 spark-forum ForumSignature 同构。
 */
export type AnnouncementSignature = {
  /** 被签名的原文（buildAnnouncementSignPayload 产物）；验签侧不回放，从记录当前字段重算比对 */
  payload: string;
  signature: string;
  publicKey: string;
};

/**
 * 公告记录（announcement_items，append-only；设计稿 §3 公告集合）。
 * 不可编辑：纠错 = 发更正公告或撤回。状态（是否已撤回）不放在本记录里，
 * 由 announcement_retractions 派生（deriveRetractionMap）——append-only
 * 集合不做覆盖，状态是派生量。
 */
export type Announcement = {
  id: string;
  orgId: string;
  kind: AnnouncementKind;
  title: string;
  body: string;
  /** 版本公告可关联版本号（档一-2：MVP 手动自由字段，展示级） */
  version?: string;
  /** 版本公告可关联发布记录引用（档三-24：发布单集合记录 id，自由字段展示级） */
  releaseRef?: string;
  publisherRootId: string;
  publishedAt: number;
  signature?: AnnouncementSignature;
};

/**
 * 撤回记录（announcement_retractions，append-only；设计稿 §3 撤回集合）。
 * 独立集合而非改原记录，与 append-only 纪律一致；已扩散内容不可收回，
 * 各成员本机实例同步后把对应公告卡片/详情标注为「已撤回」。
 */
export type AnnouncementRetraction = {
  id: string;
  orgId: string;
  targetAnnouncementId: string;
  reason?: string;
  retractorRootId: string;
  retractedAt: number;
  signature?: AnnouncementSignature;
};

/**
 * 发布权配置（announcement_config，lww；设计稿 §3 配置集合）。
 * 发布者域身份集合 + 类型开关；档三-23：MVP 由组织管理员直改初始化，
 * 后续变更挂组织治理事务（排「规则挂事务」迭代）。
 */
export type AnnouncementConfig = {
  orgId: string;
  /** 发布权集合：域身份 rootId 列表（业务层校验，内核不认识「谁能发公告」） */
  publisherRootIds: string[];
  /** 类型开关：缺省视为 true（旧配置缺字段也能读写，演进纪律） */
  enableRelease?: boolean;
  enableNotice?: boolean;
  createdBy: string;
  createdAt: number;
  updatedAt: number;
};

type OrgRole = 'admin' | 'member' | null | undefined;

export function normalizeAnnouncementText(content: string): string {
  return content.trim();
}

// ------------------------------------------------------------------
// 权限（业务层校验，dev-guide §8 模式；内核不认识发布权集合）
// ------------------------------------------------------------------

/**
 * 发布权（设计稿 §2 角色表）：域身份在发布权配置登记的集合内。
 * 配置缺失（尚未初始化）时 fail-closed——任何成员都无发布路径，
 * 由名册管理员先初始化发布权集合（档三-23）。
 */
export function canPublishAnnouncement(
  config: AnnouncementConfig | null | undefined,
  rootId: string | null | undefined
): boolean {
  if (!config || !rootId) {
    return false;
  }
  return config.publisherRootIds.includes(rootId);
}

/** 发布权配置管理（档三-23：MVP 名册管理员直改；演化挂组织治理事务排后续） */
export function canManageAnnounceConfig(currentRole: OrgRole): boolean {
  return currentRole === 'admin';
}

/**
 * 撤回权：发布权集合成员或名册管理员。撤回是 append-only 留痕动作，
 * 不改变原公告记录（不可删改），只追加撤回记录。
 */
export function canRetractAnnouncement(
  config: AnnouncementConfig | null | undefined,
  rootId: string | null | undefined,
  currentRole: OrgRole
): boolean {
  return canPublishAnnouncement(config, rootId) || currentRole === 'admin';
}

// ------------------------------------------------------------------
// 校验
// ------------------------------------------------------------------

export function validateAnnouncementTitle(title: string): { ok: boolean; reason?: string } {
  const normalized = normalizeAnnouncementText(title);
  if (!normalized) {
    return { ok: false, reason: '标题不能为空' };
  }
  if (normalized.length > ANNOUNCEMENT_MAX_TITLE_LENGTH) {
    return { ok: false, reason: `标题长度不能超过${ANNOUNCEMENT_MAX_TITLE_LENGTH}字` };
  }
  return { ok: true };
}

export function validateAnnouncementBody(body: string): { ok: boolean; reason?: string } {
  const normalized = normalizeAnnouncementText(body);
  if (!normalized) {
    return { ok: false, reason: '正文不能为空' };
  }
  if (normalized.length > ANNOUNCEMENT_MAX_BODY_LENGTH) {
    return { ok: false, reason: `正文长度不能超过${ANNOUNCEMENT_MAX_BODY_LENGTH}字` };
  }
  return { ok: true };
}

/** 版本号/发布记录引用为可选自由字段（展示级），仅约束长度 */
export function validateVersionFields(
  version: string | undefined,
  releaseRef: string | undefined
): { ok: boolean; reason?: string } {
  if (version && normalizeAnnouncementText(version).length > ANNOUNCEMENT_MAX_VERSION_LENGTH) {
    return { ok: false, reason: `版本号长度不能超过${ANNOUNCEMENT_MAX_VERSION_LENGTH}字` };
  }
  if (releaseRef && normalizeAnnouncementText(releaseRef).length > ANNOUNCEMENT_MAX_RELEASE_REF_LENGTH) {
    return { ok: false, reason: `发布记录引用长度不能超过${ANNOUNCEMENT_MAX_RELEASE_REF_LENGTH}字` };
  }
  return { ok: true };
}

export function validateRetractReason(reason: string | undefined): { ok: boolean; reason?: string } {
  if (reason && normalizeAnnouncementText(reason).length > ANNOUNCEMENT_MAX_RETRACT_REASON_LENGTH) {
    return { ok: false, reason: `撤回理由长度不能超过${ANNOUNCEMENT_MAX_RETRACT_REASON_LENGTH}字` };
  }
  return { ok: true };
}

// ------------------------------------------------------------------
// 签名（四元绑定，与 spark-forum 同范式）
// ------------------------------------------------------------------

/**
 * 内容哈希（FNV-1a 32bit，hex 输出）。只需要稳定、确定性的内容指纹来压缩
 * 签名载荷长度，防抵赖强度由身份模块的 Ed25519 域签名保证；插件沙箱内不
 * 假设 WebCrypto 可用（opaque origin iframe），故用纯 TS 实现。
 */
export function hashAnnouncementContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    // 乘以 FNV 素数 16777619（用位运算避免浮点）
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/**
 * 签名载荷：`{orgId}:{recordId}:{publisherRootId}:{内容哈希}` 四元绑定
 * （spark-forum buildForumSignPayload 同模式）。签名即绑定「谁在哪个组织以
 * 哪个身份发了哪条公告/撤回」，无法被剪贴到别的记录/组织上重放。
 */
export function buildAnnouncementSignPayload(
  orgId: string,
  recordId: string,
  publisherRootId: string,
  content: string
): string {
  return `${orgId}:${recordId}:${publisherRootId}:${hashAnnouncementContent(content)}`;
}

/**
 * 公告签名内容：类型 + 标题 + 正文 + 版本号 + 发布记录引用全量编入，
 * 任一字段被替换即验签失配。
 */
export function announcementSignContent(input: {
  kind: AnnouncementKind;
  title: string;
  body: string;
  version?: string;
  releaseRef?: string;
}): string {
  return `${input.kind}\n${input.title}\n${input.body}\n${input.version ?? ''}\n${input.releaseRef ?? ''}`;
}

/** 撤回签名内容：绑定「谁撤回了哪条公告、理由是什么」 */
export function retractionSignContent(targetAnnouncementId: string, reason?: string): string {
  return `${targetAnnouncementId}:${reason ?? ''}`;
}

// ------------------------------------------------------------------
// summary 纪律（设计稿 §3：≤200 字符、自含完整语义，未装插件时壳层原生渲染）
// ------------------------------------------------------------------

/**
 * 应用消息摘要（声明式降级文本）：`【版本公告·vX】标题：正文首部…` /
 * `【团队通知】标题：正文首部…`。由公告标题 + 正文首部确定性截取，trim 后
 * ≤200 字符，自成完整一句话——这是未装插件时壳层原生渲染的保底文本。
 * 截断按码点（[...str]）而非 UTF-16 码元，surrogate 对（emoji 等）不被劈开。
 */
export function buildAnnouncementSummary(input: {
  kind: AnnouncementKind;
  title: string;
  body: string;
  version?: string;
}): string {
  /** 码点安全截断到 max（超限补省略号，结果长度 = max） */
  const truncate = (text: string, max: number): string => {
    const chars = [...text];
    return chars.length <= max ? text : `${chars.slice(0, max - 1).join('')}…`;
  };

  const versionPart = input.kind === 'release' && input.version?.trim() ? `·${input.version.trim()}` : '';
  const prefix = input.kind === 'release' ? `【版本公告${versionPart}】` : '【团队通知】';
  const title = normalizeAnnouncementText(input.title);
  const bodyPreview = normalizeAnnouncementText(input.body).replace(/\s+/g, ' ');

  let summary = `${prefix}${title}`;
  if ([...summary].length > ANNOUNCEMENT_SUMMARY_LIMIT) {
    return truncate(summary, ANNOUNCEMENT_SUMMARY_LIMIT);
  }
  // 余量留给正文首部：标题后接「：正文…」，至少要有放一个字符加省略号的空间才拼接
  const remaining = ANNOUNCEMENT_SUMMARY_LIMIT - [...summary].length;
  if (bodyPreview && remaining > 2) {
    const budget = remaining - 1; // 「：」占位
    const bodyChars = [...bodyPreview];
    const excerpt = bodyChars.slice(0, budget).join('');
    summary += `：${excerpt}${bodyChars.length > budget ? '…' : ''}`;
    if ([...summary].length > ANNOUNCEMENT_SUMMARY_LIMIT) {
      summary = truncate(summary, ANNOUNCEMENT_SUMMARY_LIMIT);
    }
  }
  return summary;
}

/**
 * 历史公告汇总摘要（设计稿 §3 节流补发：超过阈值的历史公告只补最新一条 +
 * 「另有 N 条历史公告」汇总，防刷屏）。summary 同样自含完整语义。
 */
export function buildHistorySummary(count: number): string {
  return `【公告】另有 ${count} 条历史公告未逐条推送，请打开「公告」插件查看完整列表。`;
}

// ------------------------------------------------------------------
// 撤回派生与列表组装
// ------------------------------------------------------------------

/**
 * 从撤回记录流派生「公告 id → 最新撤回记录」映射（派生量，不落库）。
 * append-only 集合挡不住自制客户端写入伪造撤回——读侧鉴权由调用方传入
 * 的合法撤回人集合（发布权集合 ∪ 名册管理员）把关：非法撤回人的记录留痕
 * 但不参与派生；集合为空 = 全部忽略（fail-closed）。与 spark-forum
 * deriveTopicState 的读侧鉴权同口径。
 *
 * 排序确定性：retractedAt 平局（跨设备时钟不齐）时按 id 字典序二次排序。
 */
export function deriveRetractionMap(
  retractions: AnnouncementRetraction[],
  retractorRootIds: ReadonlySet<string>
): Map<string, AnnouncementRetraction> {
  const ordered = retractions
    .filter((item) => retractorRootIds.has(item.retractorRootId))
    .sort((a, b) => a.retractedAt - b.retractedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  const map = new Map<string, AnnouncementRetraction>();
  for (const item of ordered) {
    // 同一目标多条撤回：按时间序覆盖，保留最新一条（append-only 语义下的「状态」）
    map.set(item.targetAnnouncementId, item);
  }
  return map;
}

export function isAnnouncementRetracted(
  announcementId: string,
  retractionMap: ReadonlyMap<string, AnnouncementRetraction>
): boolean {
  return retractionMap.has(announcementId);
}

/**
 * orgId 复核防御：撤回记录的 orgId 是自报字段，派生映射不过滤目标组织——
 * 应用撤回标注前必须复核撤回记录与公告同属一个组织（跨组织伪造撤回无法
 * 给本组织公告盖章）。查不到/不匹配返回 undefined（fail-closed）。
 */
export function findApplicableRetraction(
  announcement: Pick<Announcement, 'id' | 'orgId'>,
  retractionMap: ReadonlyMap<string, AnnouncementRetraction>
): AnnouncementRetraction | undefined {
  const retraction = retractionMap.get(announcement.id);
  return retraction && retraction.orgId === announcement.orgId ? retraction : undefined;
}

/** 列表项（视图层用：公告 + 撤回标注） */
export type AnnouncementListItem = {
  announcement: Announcement;
  retraction?: AnnouncementRetraction;
};

/**
 * 公告列表组装：时间倒序（publishedAt 平局按 id 字典序，跨设备确定性），
 * 类型筛选可选；已撤回的不删除、由视图层置灰标注（诚实呈现：撤回标痕迹
 * 不删除）。
 */
export function buildAnnouncementList(
  announcements: Announcement[],
  retractionMap: ReadonlyMap<string, AnnouncementRetraction>,
  kindFilter?: AnnouncementKind | 'all'
): AnnouncementListItem[] {
  return announcements
    .filter((item) => !kindFilter || kindFilter === 'all' || item.kind === kindFilter)
    .sort(
      (a, b) =>
        b.publishedAt - a.publishedAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
    )
    .map((announcement) => ({
      announcement,
      retraction: findApplicableRetraction(announcement, retractionMap)
    }));
}

// ------------------------------------------------------------------
// 补发节流（设计稿 §3 限流预算，档二-4 MVP 降级：插件加载时补发 + 节流）
// ------------------------------------------------------------------

/**
 * 补发批次：待补发公告（未送达、未撤回，按发布时间升序传入）超过阈值时，
 * 只生成最新一条的卡片 + 一条「另有 N 条历史公告」汇总消息（summaryOnly 的
 * 其余公告直接记账，不逐条推卡片）；不超过阈值则逐条生成。
 */
export type BackfillBatch = {
  /** 本轮要逐条生成卡片的公告（升序） */
  cards: Announcement[];
  /** 被汇总覆盖的公告数（>0 时需追加一条汇总消息） */
  summarizedCount: number;
  /** 被汇总覆盖、直接记账的公告 */
  summarized: Announcement[];
};

export function selectBackfillBatch(pending: Announcement[]): BackfillBatch {
  if (pending.length <= ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD) {
    return { cards: pending, summarizedCount: 0, summarized: [] };
  }
  const summarized = pending.slice(0, pending.length - 1);
  return { cards: pending.slice(pending.length - 1), summarizedCount: summarized.length, summarized };
}
