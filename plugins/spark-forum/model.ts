/**
 * 论坛插件（spark-forum）· 数据模型与纯函数。
 *
 * 设计依据：wiki/product/bootstrap-plugins/forum.md v0.2 §3（数据模型）。
 *
 * 与 spark-example 的关系（§5 复用评估）：
 * - 直接复用：FNV-1a 内容哈希、签名载荷四元绑定、两级评论树组装、
 *   长度参数集中在 model 层的写法、append-only/lww 选型理由；
 * - 改造：发帖权限从「仅管理员」反转为「全员可发主题与回复」，管理员权限
 *   移到板块管理与主题治理；长度约束从 260 字微博体放宽为主题 20000 /
 *   回复 5000；数据形状从平铺时间线变为 board → topic → reply 三层；
 * - 全新集合域 forum_* / plugin:spark-forum，不做 spark-example 旧数据迁移。
 *
 * 文档结构演进纪律（沿用 spark-example 头注）：只加可选字段、不改不删既有
 * 字段——signature / supersedesId / tags 等均为可选新增，旧文档缺这些字段
 * 也能正常读写。
 */

/** 主题帖正文上限（档三-9：MVP 纯文本；超限长文后续走 sdk.content blob） */
export const FORUM_MAX_TOPIC_CONTENT_LENGTH = 20000;
/** 回复正文上限 */
export const FORUM_MAX_REPLY_CONTENT_LENGTH = 5000;
/** 主题标题上限（设计稿未指定，取合理上限，集中在 model 层参数化） */
export const FORUM_MAX_TOPIC_TITLE_LENGTH = 120;
/** 板块名称/简介上限 */
export const FORUM_MAX_BOARD_NAME_LENGTH = 40;
export const FORUM_MAX_BOARD_INTRO_LENGTH = 200;
/** 治理事件理由上限 */
export const FORUM_MAX_EVENT_REASON_LENGTH = 200;

/** 应用消息摘要中标题预览的最大字数（summary 上限 200 字符，留足前缀余量） */
export const TOPIC_SUMMARY_PREVIEW_LENGTH = 80;

/** 应用消息摘要硬性上限（壳层 summary 约束；板块名脏数据超长时也不得突破） */
export const TOPIC_SUMMARY_MAX_LENGTH = 200;

/** 查询上限（无分页的 MVP 口径；达到上限时视图层提示「仅显示最近 N 条」） */
export const FORUM_QUERY_LIMITS = {
  boards: 200,
  topics: 1000,
  replies: 5000,
  topicEvents: 2000
} as const;

/** 正文格式（档三-9：MVP 仅纯文本；markdown 随渲染选型后续加） */
export type ForumContentFormat = 'plain';

/**
 * 签名信息（identity:sign 防抵赖）。随记录存储：签名出自插件域身份
 * （域私钥永不离开内核），任何成员拿到 payload + signature + publicKey
 * 都可用 identity.verify 免权限验签。
 */
export type ForumSignature = {
  /** 被签名的原文（buildForumSignPayload 产物）；验签侧不回放，从记录当前字段重算比对 */
  payload: string;
  signature: string;
  publicKey: string;
};

/** 板块记录（forum_boards，lww：可改简介/归档，覆盖语义符合「当前生效配置」直觉） */
export type ForumBoard = {
  id: string;
  orgId: string;
  name: string;
  intro: string;
  /** 排序权重（越小越靠前） */
  sort: number;
  archived: boolean;
  createdBy: string;
  createdAt: number;
  updatedAt: number;
};

/**
 * 主题帖记录（forum_topics，append-only）。
 * 编辑语义：「编辑」= 发一条 supersedesId 指向旧版的新记录，视图只展示
 * 最新版（resolveLatestTopics），历史版本仍在链上可审计——不留「事后改帖」
 * 的暗箱。状态（置顶/精华/已解决/关闭/隐藏）不放在本记录里，由
 * forum_topic_events 按时间序推导（deriveTopicState）——append-only 集合
 * 不做覆盖，状态是派生量。
 */
export type ForumTopic = {
  id: string;
  orgId: string;
  boardId: string;
  title: string;
  content: string;
  contentFormat: ForumContentFormat;
  tags?: string[];
  authorRootId: string;
  createdAt: number;
  signature?: ForumSignature;
  /** 编辑=新版本：指向被替代的旧版本 id（首版无此字段） */
  supersedesId?: string;
  replyCountSnapshot?: number;
};

/** 回复记录（forum_replies，append-only；两级楼中楼经 parentReplyId 组装） */
export type ForumReply = {
  id: string;
  orgId: string;
  topicId: string;
  parentReplyId?: string;
  content: string;
  authorRootId: string;
  createdAt: number;
  signature?: ForumSignature;
};

export type ForumReplyNode = {
  reply: ForumReply;
  replies: ForumReply[];
};

/**
 * 主题状态事件（forum_topic_events，append-only；档三-10：它是内容管理动作
 * 非决议，不声明 governance:true）。治理动作全部留痕可审计，operator 可签名。
 * hide/unhide 对应删除诉求的拍板口径（档三-14）：append-only 下没有真删除，
 * 以「关闭 + 隐藏」事件表达，且提交时如实告知不可收回。
 */
export type ForumTopicEventKind =
  | 'pin'
  | 'unpin'
  | 'feature'
  | 'unfeature'
  | 'resolve'
  | 'close'
  | 'reopen'
  | 'hide'
  | 'unhide';

export type ForumTopicEvent = {
  id: string;
  orgId: string;
  topicId: string;
  kind: ForumTopicEventKind;
  operatorRootId: string;
  reason?: string;
  createdAt: number;
  signature?: ForumSignature;
};

/** 由事件流派生的主题状态（派生量，不落库） */
export type ForumTopicState = {
  pinned: boolean;
  featured: boolean;
  resolved: boolean;
  closed: boolean;
  hidden: boolean;
};

export const DEFAULT_TOPIC_STATE: ForumTopicState = {
  pinned: false,
  featured: false,
  resolved: false,
  closed: false,
  hidden: false
};

type ForumRole = 'admin' | 'member' | null | undefined;

/**
 * 板块治理/主题治理权限（档三-11：MVP 读内核名册管理员角色；版主权排
 * 门槛插件体系就位后）。
 */
export function canManageBoards(currentRole: ForumRole): boolean {
  return currentRole === 'admin';
}

/** 主题治理操作（置顶/加精/已解决/关闭/隐藏）与板块管理同源：名册管理员 */
export function canModerateTopics(currentRole: ForumRole): boolean {
  return currentRole === 'admin';
}

/**
 * 发主题/回帖权限（§5 需改造：从 spark-example「仅管理员发帖」反转为
 * 全员可发主题与回复；非组织成员无写入入口）。
 */
export function canPostTopic(currentRole: ForumRole): boolean {
  return currentRole === 'admin' || currentRole === 'member';
}

export function canReplyTopic(currentRole: ForumRole): boolean {
  return canPostTopic(currentRole);
}

/** 编辑=新版本：只有作者本人可对自己的主题发新版本 */
export function canEditTopic(topic: ForumTopic, currentRootId: string | null | undefined): boolean {
  return !!currentRootId && topic.authorRootId === currentRootId;
}

export function normalizeForumText(content: string): string {
  return content.trim();
}

export function validateTopicTitle(title: string): { ok: boolean; reason?: string } {
  const normalized = normalizeForumText(title);
  if (!normalized) {
    return { ok: false, reason: '标题不能为空' };
  }
  if (normalized.length > FORUM_MAX_TOPIC_TITLE_LENGTH) {
    return { ok: false, reason: `标题长度不能超过${FORUM_MAX_TOPIC_TITLE_LENGTH}字` };
  }
  return { ok: true };
}

export function validateTopicContent(content: string): { ok: boolean; reason?: string } {
  const normalized = normalizeForumText(content);
  if (!normalized) {
    return { ok: false, reason: '正文不能为空' };
  }
  if (normalized.length > FORUM_MAX_TOPIC_CONTENT_LENGTH) {
    return { ok: false, reason: `正文长度不能超过${FORUM_MAX_TOPIC_CONTENT_LENGTH}字` };
  }
  return { ok: true };
}

export function validateReplyContent(content: string): { ok: boolean; reason?: string } {
  const normalized = normalizeForumText(content);
  if (!normalized) {
    return { ok: false, reason: '回复不能为空' };
  }
  if (normalized.length > FORUM_MAX_REPLY_CONTENT_LENGTH) {
    return { ok: false, reason: `回复长度不能超过${FORUM_MAX_REPLY_CONTENT_LENGTH}字` };
  }
  return { ok: true };
}

export function validateBoardInput(name: string, intro: string): { ok: boolean; reason?: string } {
  const normalizedName = normalizeForumText(name);
  if (!normalizedName) {
    return { ok: false, reason: '板块名称不能为空' };
  }
  if (normalizedName.length > FORUM_MAX_BOARD_NAME_LENGTH) {
    return { ok: false, reason: `板块名称不能超过${FORUM_MAX_BOARD_NAME_LENGTH}字` };
  }
  if (normalizeForumText(intro).length > FORUM_MAX_BOARD_INTRO_LENGTH) {
    return { ok: false, reason: `板块简介不能超过${FORUM_MAX_BOARD_INTRO_LENGTH}字` };
  }
  return { ok: true };
}

/**
 * 内容哈希（FNV-1a 32bit，hex 输出）。这里只需要一个稳定、确定性的内容
 * 指纹来压缩签名载荷长度，防抵赖强度由身份模块的 Ed25519 域签名保证，
 * 不依赖本哈希的抗碰撞性；插件沙箱内不假设 WebCrypto 可用（opaque origin
 * iframe），故用纯 TS 实现。（直接复用 spark-example hashPostContent）
 */
export function hashForumContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    // 乘以 FNV 素数 16777619（用位运算避免浮点）
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/**
 * 签名载荷：`{orgId}:{recordId}:{authorRootId}:{内容哈希}` 四元绑定
 * （直接复用 spark-example buildPostSignPayload 模式）。把组织、记录 id、
 * 作者与内容指纹全部编进载荷：签名即绑定「谁在哪个组织以哪个身份写了哪条
 * 内容」，无法被剪贴到别的记录/组织上重放，也无法在保留签名的前提下替换
 * 作者字段（验签侧用记录当前字段重算载荷比对，见 service.verifyTopicSignature 等）。
 */
export function buildForumSignPayload(
  orgId: string,
  recordId: string,
  authorRootId: string,
  content: string
): string {
  return `${orgId}:${recordId}:${authorRootId}:${hashForumContent(content)}`;
}

/**
 * 应用消息摘要（声明式降级文本，summary 强制、≤200 字符、自成一体——未装
 * 插件的成员设备上壳层原生渲染这段纯文本）。按事件类型分化并带板块名
 * （§5 需改造 buildPostSummary）；档三-12：回复不通知，故只有新主题摘要。
 */
export function buildTopicSummary(boardName: string, title: string): string {
  const normalizedBoard = normalizeForumText(boardName) || '未分板块';
  const normalizedTitle = normalizeForumText(title);
  // 板块名来自同步面（脏数据可能超长）：先给标题预览留预算，板块名按余量截断，
  // 保证摘要总长恒 ≤ TOPIC_SUMMARY_MAX_LENGTH（壳层 summary 硬约束）。
  const affixLength = '【新主题·】'.length + 1; // 前缀固定字符 + 省略号余量
  const boardBudget = Math.max(
    1,
    TOPIC_SUMMARY_MAX_LENGTH - affixLength - Math.min(TOPIC_SUMMARY_PREVIEW_LENGTH, normalizedTitle.length)
  );
  const board =
    normalizedBoard.length > boardBudget ? `${normalizedBoard.slice(0, boardBudget - 1)}…` : normalizedBoard;
  const prefix = `【新主题·${board}】`;
  const budget = Math.max(0, TOPIC_SUMMARY_MAX_LENGTH - prefix.length - 1);
  const preview = normalizedTitle.slice(0, budget);
  const ellipsis = normalizedTitle.length > budget ? '…' : '';
  return `${prefix}${preview}${ellipsis}`;
}

/**
 * 两级楼中楼组装（直接复用 spark-example buildCommentThread 逻辑，
 * 字段改名 postId→topicId / parentCommentId→parentReplyId）：
 * topicId 过滤 + parentReplyId 分桶 + createdAt 排序。
 * 超过两级的 parent 引用（脏数据）归并到其根楼层下，不丢内容。
 */
export function buildReplyThread(topicId: string, replies: ForumReply[]): ForumReplyNode[] {
  const forTopic = replies
    .filter((item) => item.topicId === topicId)
    .sort((a, b) => a.createdAt - b.createdAt);

  const roots = forTopic.filter((item) => !item.parentReplyId);
  const rootIds = new Set(roots.map((item) => item.id));
  const repliesByParent = new Map<string, ForumReply[]>();

  for (const reply of forTopic) {
    if (!reply.parentReplyId) {
      continue;
    }
    // 两级楼中楼：父引用必须是根楼层，否则归并到根楼层自身（防脏数据悬空）
    const parentId = rootIds.has(reply.parentReplyId) ? reply.parentReplyId : undefined;
    if (!parentId) {
      roots.push(reply);
      rootIds.add(reply.id);
      continue;
    }
    const bucket = repliesByParent.get(parentId) ?? [];
    bucket.push(reply);
    repliesByParent.set(parentId, bucket);
  }

  return roots.map((root) => ({
    reply: root,
    replies: (repliesByParent.get(root.id) ?? []).sort((a, b) => a.createdAt - b.createdAt)
  }));
}

/**
 * 从事件流按时间序推导主题状态（派生量）。close 同时清除 resolved；
 * reopen 只撤销 closed/hidden 之外的关闭态（hidden 由 unhide 单独撤销）。
 *
 * 读侧鉴权（档三-11 的读侧闭环）：治理动作写侧由 service.createTopicEvent
 * 拦截非管理员，但 append-only 集合并不能阻止恶意成员用自制客户端写入
 * 伪造事件——因此派生时必须再校验 operatorRootId 是否为名册管理员
 * （adminRootIds 由视图层从内核名册算出）。非管理员操作者的事件被忽略
 * （记录仍留痕在集合里，只是不参与派生）；adminRootIds 为空 = 全部忽略
 * （fail-closed）。
 *
 * 排序确定性：createdAt 平局（跨设备时钟不齐）时按 event.id 字典序二次
 * 排序，保证各设备派生结果一致。
 */
export function deriveTopicState(
  topicId: string,
  events: ForumTopicEvent[],
  adminRootIds: ReadonlySet<string>
): ForumTopicState {
  const state: ForumTopicState = { ...DEFAULT_TOPIC_STATE };
  const ordered = events
    .filter((event) => event.topicId === topicId)
    .sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  for (const event of ordered) {
    if (!adminRootIds.has(event.operatorRootId)) {
      // 非名册管理员的治理事件：留痕但忽略
      continue;
    }
    switch (event.kind) {
      case 'pin':
        state.pinned = true;
        break;
      case 'unpin':
        state.pinned = false;
        break;
      case 'feature':
        state.featured = true;
        break;
      case 'unfeature':
        state.featured = false;
        break;
      case 'resolve':
        state.resolved = true;
        break;
      case 'close':
        state.closed = true;
        state.resolved = false;
        break;
      case 'reopen':
        state.closed = false;
        break;
      case 'hide':
        state.hidden = true;
        break;
      case 'unhide':
        state.hidden = false;
        break;
    }
  }
  return state;
}

/**
 * 编辑=新版本的版本链解析：supersedesId 指回旧版，同一谱系（沿 supersedesId
 * 链回溯到首版）只保留 createdAt 最新的记录作为「当前版」。
 *
 * 防抢链：链上版本必须同属一个作者——回溯时遇 authorRootId 变更即断链，
 * 变更点起的记录视同脏链独立成谱系。否则恶意成员可发 supersedesId 指向
 * 他人主题最新版的记录抢占头部（写侧 service.createTopic 也拦，但
 * append-only 集合挡不住自制客户端，读侧必须自证）。脏链（supersedesId
 * 指向不存在的记录）同样按独立谱系处理，不丢数据。
 */
export function resolveLatestTopics(topics: ForumTopic[]): ForumTopic[] {
  const byId = new Map(topics.map((topic) => [topic.id, topic]));
  const lineageRootOf = (topic: ForumTopic): string => {
    let current = topic;
    const seen = new Set<string>([current.id]);
    while (
      current.supersedesId &&
      byId.has(current.supersedesId) &&
      !seen.has(current.supersedesId) &&
      byId.get(current.supersedesId)!.authorRootId === topic.authorRootId
    ) {
      seen.add(current.supersedesId);
      current = byId.get(current.supersedesId)!;
    }
    return current.id;
  };

  const latestByLineage = new Map<string, ForumTopic>();
  for (const topic of topics) {
    const root = lineageRootOf(topic);
    const existing = latestByLineage.get(root);
    if (!existing || topic.createdAt >= existing.createdAt) {
      latestByLineage.set(root, topic);
    }
  }
  return [...latestByLineage.values()];
}

/**
 * 某主题当前版的完整版本历史（旧→新），供「历史版本」视图展示。
 * 与 resolveLatestTopics 同一防抢链口径：回溯/前推遇 authorRootId 变更
 * 即断链，他人伪造的「新版本」不进历史。
 */
export function topicVersionHistory(current: ForumTopic, topics: ForumTopic[]): ForumTopic[] {
  const byId = new Map(topics.map((topic) => [topic.id, topic]));
  const childrenByParent = new Map<string, ForumTopic[]>();
  for (const topic of topics) {
    if (!topic.supersedesId) {
      continue;
    }
    const bucket = childrenByParent.get(topic.supersedesId) ?? [];
    bucket.push(topic);
    childrenByParent.set(topic.supersedesId, bucket);
  }

  // 从当前版回溯到首版（遇作者变更断链）
  const chain: ForumTopic[] = [];
  let cursor: ForumTopic | undefined = current;
  const seen = new Set<string>();
  while (cursor && !seen.has(cursor.id)) {
    seen.add(cursor.id);
    chain.unshift(cursor);
    const parent = cursor.supersedesId ? byId.get(cursor.supersedesId) : undefined;
    cursor = parent && parent.authorRootId === current.authorRootId ? parent : undefined;
  }
  // 首版之后若还有更新版本（说明传入的不是最新版），沿链前推（同样断链）
  let head = chain[chain.length - 1];
  while (head) {
    const next = (childrenByParent.get(head.id) ?? [])
      .filter((child) => child.authorRootId === current.authorRootId)
      .sort((a, b) => b.createdAt - a.createdAt)[0];
    if (!next || seen.has(next.id)) {
      break;
    }
    seen.add(next.id);
    chain.push(next);
    head = next;
  }
  // chain 已是 旧→新 顺序（回溯段 unshift、前推段 push）
  return chain;
}

/** 主题列表排序：隐藏主题沉底之外的视图规则——置顶优先 + 最近活跃（最近回复/创建）倒序 */
export type ForumTopicListItem = {
  topic: ForumTopic;
  state: ForumTopicState;
  replyCount: number;
  lastActiveAt: number;
};

export function buildTopicList(
  boardId: string,
  topics: ForumTopic[],
  events: ForumTopicEvent[],
  replies: ForumReply[],
  adminRootIds: ReadonlySet<string>
): ForumTopicListItem[] {
  const latest = resolveLatestTopics(topics).filter((topic) => topic.boardId === boardId);

  const replyStats = new Map<string, { count: number; lastAt: number }>();
  for (const reply of replies) {
    const stat = replyStats.get(reply.topicId) ?? { count: 0, lastAt: 0 };
    stat.count += 1;
    stat.lastAt = Math.max(stat.lastAt, reply.createdAt);
    replyStats.set(reply.topicId, stat);
  }

  const items = latest.map((topic) => {
    const stat = replyStats.get(topic.id) ?? { count: 0, lastAt: 0 };
    return {
      topic,
      state: deriveTopicState(topic.id, events, adminRootIds),
      replyCount: stat.count,
      lastActiveAt: Math.max(topic.createdAt, stat.lastAt)
    };
  });

  return items.sort((a, b) => {
    // 隐藏主题（档三-14 删除诉求表达）不参与常规列表
    if (a.state.hidden !== b.state.hidden) {
      return a.state.hidden ? 1 : -1;
    }
    if (a.state.pinned !== b.state.pinned) {
      return a.state.pinned ? -1 : 1;
    }
    return b.lastActiveAt - a.lastActiveAt;
  });
}

/**
 * 议题引用（§5 需新增，MVP 基础展示级）：帖内粘贴 `affair:<id>` 形态的
 * 议题/事务 ID，渲染为链接卡片样式的纯文本引用。插件间契约未实现
 * （档一-1），MVP 退化为纯文本展示，不跳转。
 */
export type ForumContentSegment = { type: 'text' | 'affair-ref'; text: string };

const AFFAIR_REF_PATTERN = /affair:[A-Za-z0-9_-]{4,}/g;

export function splitAffairRefs(content: string): ForumContentSegment[] {
  const segments: ForumContentSegment[] = [];
  let lastIndex = 0;
  for (const match of content.matchAll(AFFAIR_REF_PATTERN)) {
    const index = match.index ?? 0;
    if (index > lastIndex) {
      segments.push({ type: 'text', text: content.slice(lastIndex, index) });
    }
    segments.push({ type: 'affair-ref', text: match[0] });
    lastIndex = index + match[0].length;
  }
  if (lastIndex < content.length) {
    segments.push({ type: 'text', text: content.slice(lastIndex) });
  }
  return segments;
}
