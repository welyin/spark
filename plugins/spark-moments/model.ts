/**
 * 朋友圈插件（spark-moments）· 数据模型与纯函数层。
 *
 * 对齐：wiki/product/moments.md（§五 可见性、§六 投递与互动）、
 *       wiki/ui/moments/README.md（落地要点 §9：model.ts 收敛类型与纯函数）。
 *
 * 职责约定：
 * - 本文件不依赖 SDK / Vue，全部是可单测的纯函数与类型——插件业务规则
 *   （长度约束、可见性四选一展开、互动广播名单计算、删除级联、签名载荷、
 *   消息摘要、互动 key）尽量沉淀在这一层，service/视图只做编排与呈现；
 * - 集合名 `moments:*`（插件前缀须与 id `spark-moments` 一致，内核强制），
 *   与 manifest 权限声明一一对应（storage/identity/contact/feed/message）。
 *
 * 产品修正（对齐架构师 [social-feed] §10 偏差表第 1 条）：
 * 三个集合 scope 一律 "sync"（personal 空间 = 自设备间 pdsync 全量同步），
 * 不用 "local"（"local" 语义即不同步，与多设备验收口径矛盾）。
 */

// ---------------------------------------------------------------------------
// 常量与限额（对齐产品 §6.4 与 UI 设计 §6 限额速查）
// ---------------------------------------------------------------------------

/** 动态文字上限（字） */
export const MOMENTS_MAX_TEXT_LENGTH = 1000;
/** 评论文字上限（字） */
export const MOMENTS_MAX_COMMENT_LENGTH = 500;
/** 单条动态图片数量上限 */
export const MOMENTS_MAX_IMAGES = 9;
/** 单张图片体积上限（字节，10MB） */
export const MOMENTS_MAX_IMAGE_BYTES = 10 * 1024 * 1024;
/** 缩略图目标尺寸（~256×256，JPEG 质量 70%） */
export const MOMENTS_THUMB_SIZE = 256;
/** 缩略图 JPEG 质量 */
export const MOMENTS_THUMB_QUALITY = 0.7;
/** 应用消息 summary 上限（字符，内核强制 ≤200） */
export const MOMENTS_SUMMARY_LIMIT = 200;
/** 时间线每页条数 */
export const MOMENTS_PAGE_SIZE = 20;
/** 动态摘要预览最大字数（summary/卡片摘要用） */
export const MOMENTS_EXCERPT_LENGTH = 60;
/** 评论摘录最大字数（通知卡片评论类用） */
export const MOMENTS_COMMENT_EXCERPT_LENGTH = 40;

// ---------------------------------------------------------------------------
// 集合名
// ---------------------------------------------------------------------------

export const MOMENTS_COLLECTIONS = {
  posts: 'spark-moments:posts',
  interactions: 'spark-moments:interactions',
  /** 本地 profile（E6 降级：我的昵称/头像，pdsync 自设备同步） */
  profile: 'spark-moments:profile'
} as const;

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/** 「谁可以看」四选一（对齐产品 §5.3，四选一不可组合） */
export type MomentsVisibleScope = 'all' | 'private' | 'partial' | 'exclude';

/** 图片引用（缩略图 + 完整图，均经 saveBlob 入本机库；投递只携带 hash 不随信二进制） */
export type MomentsImage = {
  /** 完整图 hash（readBlob 按需拉取，≤10MB） */
  hash: string;
  /** 缩略图 hash（~256×256，KB 级即时出图，九宫格用） */
  thumbHash: string;
  name: string;
  size: number;
  mime: string;
};

/** 记录签名（identity:sign 防伪造；authorRootId 与签名公钥推导 rootId 一致性由验签保证） */
export type MomentsSignature = {
  payload: string;
  signature: string;
  publicKey: string;
};

/** 动态正文（spark-moments:posts，lww-record） */
export type MomentsPost = {
  id: string;
  authorRootId: string;
  text: string;
  images: MomentsImage[];
  createdAt: number;
  /** 可见性声明（发送方裁决，不在名单内的人根本收不到） */
  visibleScope: MomentsVisibleScope;
  /** 勾选项展开后的 rootId 名单（public 时为全部联系人、private 为空） */
  visibleList: string[];
  /** 发动态时按可见性展开的实际投递名单 rootId 列表（互动广播 / 删除级联依据） */
  recipients: string[];
  /** 作者软删除标记；非空 = 已删除（本地 + 接收方级联） */
  deletedAt?: number;
  /** 作者快照（非联系人时展示用：备注>昵称、头像）；发动态时由本机 profile/通讯录解析 */
  authorSnapshot?: { nickname: string; avatar?: string };
  /** 发动态时的域身份签名（identity:sign 防伪造；用户拒绝授权或旧版本动态则无此字段） */
  signature?: MomentsSignature;
};

/** 互动（点赞 + 评论，spark-moments:interactions，lww-record，key 复合） */
export type MomentsInteraction = {
  type: 'like' | 'comment';
  /** 评论正文（仅 comment 类；like 类无） */
  text?: string;
  /** add = 点赞/评论；remove = 取消赞/删评论（互动广播 payload 用） */
  action: 'add' | 'remove';
  ts: number;
};

// ---------------------------------------------------------------------------
// feed payload 线形（投递/收件用，topic 见 service.MOMENTS_TOPICS）
// ---------------------------------------------------------------------------
// 线形约定：三通道 payload 一律 JSON 字符串，携带数据 + 签名（payload/signature/
// publicKey）三元组；收发双方都用 model 层编解码往返，验签侧从解码后的字段
// 重算载荷比对（identity:verify），防剪贴重放、防字段被替换后带签重放。

/** 动态投递 payload（post 通道：`spark-moments:post`） */
export type MomentsPostPayload = {
  post: MomentsPost;
  sig: string;
  pubKey: string;
};

/** 互动投递 payload（interaction 通道：`spark-moments:interaction`） */
export type MomentsInteractionPayload = {
  postId: string;
  interaction: MomentsInteraction;
  /** 广播名单：`"author"` = 作者收件（第一跳）；否则为逗号分隔 rootId 名单（第二跳广播） */
  broadcast: string[] | 'author';
  sig: string;
  pubKey: string;
};

/** 删除通知 payload（delete 通道：`spark-moments:delete`） */
export type MomentsDeletePayload = {
  postId: string;
  authorRootId: string;
  sig: string;
  pubKey: string;
};

/** 本地 profile 记录（E6 降级，spark-moments:profile，lww-record，key 恒 "self"） */
export type MomentsProfile = {
  nickname: string;
  avatar?: string;
  updatedAt: number;
};

// ---------------------------------------------------------------------------
// 纯函数：文本校验
// ---------------------------------------------------------------------------

export function normalizeMomentsText(content: string): string {
  return content.trim();
}

export function validateMomentsText(content: string): { ok: boolean; reason?: string } {
  const normalized = normalizeMomentsText(content);
  if (!normalized) {
    return { ok: false, reason: '内容不能为空' };
  }
  if (normalized.length > MOMENTS_MAX_TEXT_LENGTH) {
    return { ok: false, reason: `内容长度不能超过${MOMENTS_MAX_TEXT_LENGTH}字` };
  }
  return { ok: true };
}

export function validateCommentText(content: string): { ok: boolean; reason?: string } {
  const normalized = normalizeMomentsText(content);
  if (!normalized) {
    return { ok: false, reason: '评论不能为空' };
  }
  if (normalized.length > MOMENTS_MAX_COMMENT_LENGTH) {
    return { ok: false, reason: `评论长度不能超过${MOMENTS_MAX_COMMENT_LENGTH}字` };
  }
  return { ok: true };
}

/** 图片校验：1–9 张，单张 ≤10MB（对齐产品 §6.4） */
export function validateImages(images: MomentsImage[]): { ok: boolean; reason?: string } {
  if (images.length < 1) {
    return { ok: false, reason: '至少需要 1 张图片' };
  }
  if (images.length > MOMENTS_MAX_IMAGES) {
    return { ok: false, reason: `单条动态最多 ${MOMENTS_MAX_IMAGES} 张图片` };
  }
  const oversize = images.find((img) => img.size > MOMENTS_MAX_IMAGE_BYTES);
  if (oversize) {
    return { ok: false, reason: `单张图片不能超过 ${Math.floor(MOMENTS_MAX_IMAGE_BYTES / 1024 / 1024)}MB` };
  }
  return { ok: true };
}

// ---------------------------------------------------------------------------
// 纯函数：id / 时间
// ---------------------------------------------------------------------------

export function newId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

/**
 * 动态 id：`moment-{authorRootId前8}-{ts}-{rand}`。
 * 确定性前缀（作者 + 时间戳）保证同一作者同一时刻不碰撞，随机尾缀跨作者唯一；
 * 前 8 位截断作者 rootId 用于可读性与来源溯源，不依赖完整 rootId。
 */
export function buildMomentId(authorRootId: string, ts: number, rand?: string): string {
  const authorPrefix = authorRootId.slice(0, 8);
  const randPart = rand ?? Math.random().toString(16).slice(2, 10);
  return `moment-${authorPrefix}-${ts}-${randPart}`;
}

/** 相对时间（对齐消息文档口径）：刚刚 / N 分钟前 / 今天 HH:mm / 昨天 / M/D / YYYY/M/D */
export function formatRelativeTime(timestamp: number, now = Date.now()): string {
  const diff = now - timestamp;
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  const d = new Date(timestamp);
  const today = new Date(now);
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (sameDay(d, today)) return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  const yesterday = new Date(now - 86_400_000);
  if (sameDay(d, yesterday)) return '昨天';
  if (d.getFullYear() === today.getFullYear()) return `${d.getMonth() + 1}/${d.getDate()}`;
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

/** 绝对时间（详情页）：YYYY/M/D HH:mm，当年省年份 */
export function formatAbsoluteTime(timestamp: number): string {
  const d = new Date(timestamp);
  const year = d.getFullYear() === new Date().getFullYear() ? '' : `${d.getFullYear()}/`;
  return `${year}${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

// ---------------------------------------------------------------------------
// 纯函数：内容哈希（FNV-1a 32bit，hex）——签名载荷压缩用，抗碰撞由 Ed25519 域签名保证
// ---------------------------------------------------------------------------

export function hashContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

// ---------------------------------------------------------------------------
// 纯函数：签名载荷（防剪贴重放、防作者替换）
// ---------------------------------------------------------------------------

/** 动态签名载荷：`moments:post:{postId}:{authorRootId}:{文本哈希}:{图片哈希列表}` */
export function buildPostSignPayload(post: MomentsPost): string {
  const imageHashes = post.images.map((img) => img.hash).join(',');
  return `moments:post:${post.id}:${post.authorRootId}:${hashContent(post.text)}:${hashContent(imageHashes)}`;
}

/** 互动签名载荷：`moments:interaction:{postId}:{type}:{rootId}:{文本哈希}:{action}` */
export function buildInteractionSignPayload(
  postId: string,
  type: 'like' | 'comment',
  rootId: string,
  text: string,
  action: 'add' | 'remove'
): string {
  return `moments:interaction:${postId}:${type}:${rootId}:${hashContent(text)}:${action}`;
}

/** 删除签名载荷：`moments:delete:{postId}:{authorRootId}`——绑定「作者删了哪条动态」 */
export function buildDeleteSignPayload(postId: string, authorRootId: string): string {
  return `moments:delete:${postId}:${authorRootId}`;
}

// ---------------------------------------------------------------------------
// 纯函数：可见性四选一展开（发送方裁决，对齐产品 §5.3）
// ---------------------------------------------------------------------------

/**
 * 可见性展开为投递名单 rootId 列表（去重、按输入顺序稳定）。
 *
 * - all     ：全部联系人 rootId
 * - private ：空（不投递，仅本地）
 * - partial ：勾选项展开集合（去重后）
 * - exclude ：全部联系人 − 勾选项展开集合
 *
 * 勾选项（可见列表/排除列表）来源：具体联系人 rootId + 分组/标签展开的成员 rootId，
 * 由调用方（service 层经 contacts.listFriends/listGroups/listTags 快照展开）汇总传入。
 */
export function expandRecipients(
  scope: MomentsVisibleScope,
  allContactRootIds: string[],
  selectedRootIds: string[]
): { recipients: string[]; visibleList: string[] } {
  if (scope === 'private') {
    return { recipients: [], visibleList: [] };
  }
  if (scope === 'all') {
    const recipients = dedupe(allContactRootIds);
    return { recipients, visibleList: recipients };
  }
  if (scope === 'partial') {
    const recipients = dedupe(selectedRootIds);
    return { recipients, visibleList: recipients };
  }
  // exclude：全部 − 选中
  const exclude = new Set(selectedRootIds);
  const recipients = dedupe(allContactRootIds).filter((rootId) => !exclude.has(rootId));
  return { recipients, visibleList: recipients };
}

/** 数组去重（保序） */
export function dedupe<T>(list: T[]): T[] {
  return [...new Set(list)];
}

// ---------------------------------------------------------------------------
// 纯函数：互动 key（spark-moments:interactions 复合键，产品 §6.2）
// ---------------------------------------------------------------------------

/** 互动集合键：`{postId}:{type}:{rootId}` */
export function interactionKey(postId: string, type: 'like' | 'comment', rootId: string): string {
  return `${postId}:${type}:${rootId}`;
}

// ---------------------------------------------------------------------------
// 纯函数：互动广播名单（产品 §6.2 两跳链路）
// ---------------------------------------------------------------------------

/**
 * 计算互动广播名单：作者收到互动后，向该动态投递名单广播（除互动发起者外）。
 *
 * 语义：投递名单 = 收到了这条动态的人，自然就是「共同好友」——等价微信
 * 共同好友可见评论的去中心化实现。投递名单只存作者本机，不外泄。
 */
export function computeInteractionBroadcast(
  postRecipients: string[],
  interactionRootId: string
): string[] {
  return dedupe(postRecipients).filter((rootId) => rootId !== interactionRootId);
}

// ---------------------------------------------------------------------------
// 纯函数：删除级联（产品 §6.5）
// ---------------------------------------------------------------------------

/**
 * 删除通知广播名单：作者删除动态后，向原收件人名单广播（接收方据此本地标记删除）。
 * 名单与互动广播同源（post.recipients），删动态时连带清理。
 */
export function computeDeleteBroadcast(postRecipients: string[]): string[] {
  return dedupe(postRecipients);
}

// ---------------------------------------------------------------------------
// 纯函数：应用消息摘要（声明式降级文本，cards.md §2.2）
// ---------------------------------------------------------------------------

/** 动态正文摘要（截断 + 省略号；纯图片动态显示「[图片]」） */
export function buildPostExcerpt(post: Pick<MomentsPost, 'text' | 'images'>): string {
  if (!post.text) return '[图片]';
  const normalized = post.text.trim();
  if (normalized.length <= MOMENTS_EXCERPT_LENGTH) return normalized;
  return `${normalized.slice(0, MOMENTS_EXCERPT_LENGTH)}…`;
}

/** 评论摘录（前 40 字） */
export function buildCommentExcerpt(text: string): string {
  const normalized = text.trim();
  if (normalized.length <= MOMENTS_COMMENT_EXCERPT_LENGTH) return normalized;
  return `${normalized.slice(0, MOMENTS_COMMENT_EXCERPT_LENGTH)}…`;
}

/** 互动通知 summary（≤200 字符，未装插件时壳层原生渲染） */
export function buildInteractionSummary(
  kind: 'like' | 'comment',
  fromName: string,
  count: number,
  postExcerpt: string,
  commentExcerpt?: string
): string {
  const who = count > 1 ? `${fromName} 等 ${count} 人` : fromName;
  const verb = kind === 'like' ? '赞了你的动态' : '评论了你的动态';
  if (kind === 'comment' && commentExcerpt && count === 1) {
    return `${who} ${verb}：${commentExcerpt}`.slice(0, MOMENTS_SUMMARY_LIMIT);
  }
  return `${who} ${verb}：${postExcerpt}`.slice(0, MOMENTS_SUMMARY_LIMIT);
}

// ---------------------------------------------------------------------------
// 纯函数：时间线排序与过滤
// ---------------------------------------------------------------------------

/** 时间线：按 createdAt 倒序，过滤已删除（deletedAt 非空）；返回最新在前 */
export function sortTimeline<T extends { createdAt: number; deletedAt?: number }>(items: T[]): T[] {
  return items
    .filter((item) => item.deletedAt == null)
    .sort((a, b) => b.createdAt - a.createdAt);
}

// ---------------------------------------------------------------------------
// 纯函数：勾选项展开 + 可见性裁决（「谁可以看」选择器 → 投递名单，产品 §5.3）
// ---------------------------------------------------------------------------

/**
 * 从「联系人/分组/标签」勾选项展开为投递名单（rootId 去重、按 friends 输入顺序）。
 *
 * 入参语义：
 * - scope          ：四选一可见性；
 * - selection      ：「谁可以看」勾选项原始值（联系人 rootId / 分组 groupId / 标签 tagId 混合）；
 * - friends        ：通讯录朋友摘要（含 rootId / groupId / tagIds），展开分组与标签成员的来源；
 * - groups / tags  ：通讯录分组/标签元数据（保留签名以对齐任务线形；当前展开只依赖 friends
 *                    的 groupId/tagIds 匹配，元数据留作未来校验勾选项合法性）。
 *
 * 返回：`{ allRootIds, selectedRootIds }`——全部联系人 rootId 与勾选项展开后的 rootId 集合，
 * 供 `expandRecipients` 做四选一裁决。private 返回空名单（不投递）。
 */
export function expandRecipientsFromSelection(
  scope: MomentsVisibleScope,
  selection: { contactRootIds: string[]; groupIds: string[]; tagIds: string[] },
  friends: Array<{ rootId: string; groupId: string; tagIds: string[] }>,
  _groups: unknown[] = [],
  _tags: unknown[] = []
): { allRootIds: string[]; selectedRootIds: string[] } {
  const allRootIds = friends.map((f) => f.rootId);
  if (scope === 'private') {
    return { allRootIds: [], selectedRootIds: [] };
  }

  const contactRoots = new Set(selection.contactRootIds);
  const groupIds = new Set(selection.groupIds);
  const tagIds = new Set(selection.tagIds);
  const selectedRootIds: string[] = [];

  for (const friend of friends) {
    const inGroup = groupIds.has(friend.groupId);
    const inTag = friend.tagIds.some((tagId) => tagIds.has(tagId));
    if (contactRoots.has(friend.rootId) || inGroup || inTag) {
      selectedRootIds.push(friend.rootId);
    }
  }
  return { allRootIds, selectedRootIds: dedupe(selectedRootIds) };
}

// ---------------------------------------------------------------------------
// 纯函数：feed payload 编解码（投递/收件线形，产品 §6 与 service.MOMENTS_TOPICS）
// ---------------------------------------------------------------------------
// 三通道 payload 均为 JSON 字符串；decode 返回完整结构（含 sig/pubKey），
// 验签侧从 decoded.post / decoded.interaction 字段重算载荷比对（identity:verify）。

/** 动态 payload 编码：`{post, sig, pubKey}` → JSON 字符串 */
export function encodePostPayload(post: MomentsPost, sig: string, pubKey: string): string {
  return JSON.stringify({ post, sig, pubKey });
}

/** 动态 payload 解码（验签前置：从返回的 post 重算 buildPostSignPayload 比对） */
export function decodePostPayload(payload: string): MomentsPostPayload {
  const parsed = JSON.parse(payload) as MomentsPostPayload;
  if (!parsed.post || typeof parsed.sig !== 'string' || typeof parsed.pubKey !== 'string') {
    throw new Error('invalid post payload: missing post/sig/pubKey');
  }
  return parsed;
}

/** 互动 payload 编码：`{postId, interaction, broadcast, sig, pubKey}` → JSON 字符串 */
export function encodeInteractionPayload(
  postId: string,
  interaction: MomentsInteraction,
  broadcast: MomentsInteractionPayload['broadcast'],
  sig: string,
  pubKey: string
): string {
  return JSON.stringify({ postId, interaction, broadcast, sig, pubKey });
}

/** 互动 payload 解码（验签前置：从 interaction 字段重算 buildInteractionSignPayload 比对） */
export function decodeInteractionPayload(payload: string): MomentsInteractionPayload {
  const parsed = JSON.parse(payload) as MomentsInteractionPayload;
  if (typeof parsed.postId !== 'string' || !parsed.interaction || typeof parsed.sig !== 'string' || typeof parsed.pubKey !== 'string') {
    throw new Error('invalid interaction payload: missing postId/interaction/sig/pubKey');
  }
  return parsed;
}

/** 删除通知 payload 编码：`{postId, authorRootId, sig, pubKey}` → JSON 字符串 */
export function encodeDeletePayload(postId: string, authorRootId: string, sig: string, pubKey: string): string {
  return JSON.stringify({ postId, authorRootId, sig, pubKey });
}

/** 删除通知 payload 解码（验签前置：从 postId/authorRootId 重算 buildDeleteSignPayload 比对） */
export function decodeDeletePayload(payload: string): MomentsDeletePayload {
  const parsed = JSON.parse(payload) as MomentsDeletePayload;
  if (typeof parsed.postId !== 'string' || typeof parsed.authorRootId !== 'string' || typeof parsed.sig !== 'string' || typeof parsed.pubKey !== 'string') {
    throw new Error('invalid delete payload: missing postId/authorRootId/sig/pubKey');
  }
  return parsed;
}
