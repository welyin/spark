/**
 * 朋友圈插件（spark-moments）· 后台入口（内核 QuickJS 沙箱，plugin_system.md「后台运行时」）。
 *
 * 设计职责（产品 §七/§十三）：feed 收件（三通道验签落库）+ 互动通知生成。
 * 与 iframe 主视图（useMoments.subscribeFeed/pullInbox）同口径：post/interaction/delete
 * 三 topic 均先验签后落库（防伪造硬约束），互动第一跳作者侧广播名单 + 写应用会话通知。
 * 双侧都会生成互动通知（双写者），去重靠 sdk.data scope:'local' 持久台账
 * `spark-moments:notified`（键 = interactionNotifyKey；QuickJS 无 localStorage，
 * 持久面故障时进程内 Set 兜底）——feed 重复投递 / 视图与后台并发触发不重复写应用会话。
 *
 * 当前能力（内核已补齐 background PRELUDE）：
 * - `spark.identity.verify/sign`：验签 / 域身份签名（免 import，宿主注入）；
 * - `spark.messages.sendAppMessage`：应用会话写（互动通知，message:app + 内核限流）。
 *
 * 收件链路：内核 feed 入站 → 收件箱 → `spark.feed.onReceive` 在线推送 / `spark.feed.pull`
 * 启动补读（崩溃/未运行期间进入收件箱的数据，架构 §8）。两路径均先验签后落库。
 *
 * 验签口径（与 service.ts 同）：签名用**域身份**（非 root 身份）私钥，故 pubKey 无法反推
 * rootId（域公钥 ≠ root 公钥）。防伪造一致性靠「把 authorRootId/rootId 绑进签名载荷 +
 * 重算载荷比对 + 密码学验签」保证——authorRootId/rootId 参与 buildPostSignPayload /
 * buildInteractionSignPayload / buildDeleteSignPayload，字段被替换即验签失败。
 *
 * 写作约束（QuickJS 沙箱，无 DOM/无 SDK 桥）：零运行时依赖，宿主直接 eval；
 * 集合名/topic/签名载荷等纯函数内联（与 model.ts 逐字一致，避免 import 共享代码切 chunk）。
 */

declare const spark: {
  readonly pluginId: string;
  log: (msg: string) => void;
  data: {
    declareCollection: (decl: { name: string; scope?: 'sync' | 'local' }) => unknown;
    save: (name: string, key: string, value: unknown) => unknown;
    get: (name: string, key: string) => Record<string, unknown> | null;
  };
  feed: {
    onReceive: (topic: string, handler: (msg: { topic: string; payload: unknown; from: string; ts: number }) => void) => void;
    pull: (input: { topic: string; cursor?: string; limit?: number }) => { items: Array<{ topic: string; payload: unknown; from: string; ts: number }>; nextCursor?: string };
    deliver: (input: { topic: string; payload: unknown; recipients: string[]; replyTo?: string }) => { requested: number; accepted: number };
  };
  identity: {
    sign: (payload: string) => { domain: string; domainId: string; publicKey: string; signature: string; payloadHash: string };
    verify: (input: { payload: string; sig: string; pubKey: string }) => boolean;
  };
  messages: {
    sendAppMessage: (input: { summary: string; card?: { viewId: string; data: unknown } }) => unknown;
  };
};

/** 集合名内联（与 model.ts MOMENTS_COLLECTIONS / MOMENTS_NOTIFIED_COLLECTION 逐字一致；零依赖约束故内联） */
const COLLECTIONS = {
  posts: 'spark-moments:posts',
  interactions: 'spark-moments:interactions',
  /** 互动通知去重台账（scope:'local'，不参与同步；与 iframe 视图侧共用同一持久面） */
  notified: 'spark-moments:notified'
} as const;

/** topic（与 service.ts MOMENTS_TOPICS 逐字一致） */
const TOPICS = {
  post: 'spark-moments:post',
  interaction: 'spark-moments:interaction',
  delete: 'spark-moments:delete'
} as const;

/** 收到计数（进程内；日志用） */
const receivedCounts: Record<string, number> = { post: 0, interaction: 0, delete: 0 };

/** 应用消息 summary 上限（与 model.ts MOMENTS_SUMMARY_LIMIT 一致；内核强制 ≤200） */
const SUMMARY_LIMIT = 200;
/** 动态摘要 / 评论摘录截断长度（与 model.ts 一致） */
const EXCERPT_LENGTH = 60;
const COMMENT_EXCERPT_LENGTH = 40;

// ---------------------------------------------------------------------------
// 纯函数内联（与 model.ts 逐字一致，防 import 切 chunk）
// ---------------------------------------------------------------------------

/** FNV-1a 32bit hex（签名载荷压缩用，抗碰撞由 Ed25519 域签名保证） */
function hashContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/** 动态签名载荷：`moments:post:{postId}:{authorRootId}:{文本哈希}:{图片哈希列表}` */
function buildPostSignPayload(post: { id: string; authorRootId: string; text: string; images: Array<{ hash: string }> }): string {
  const imageHashes = (post.images ?? []).map((img) => img.hash).join(',');
  return `moments:post:${post.id}:${post.authorRootId}:${hashContent(post.text)}:${hashContent(imageHashes)}`;
}

/** 互动签名载荷：`moments:interaction:{postId}:{type}:{rootId}:{文本哈希}:{action}` */
function buildInteractionSignPayload(
  postId: string,
  type: 'like' | 'comment',
  rootId: string,
  text: string,
  action: 'add' | 'remove'
): string {
  return `moments:interaction:${postId}:${type}:${rootId}:${hashContent(text)}:${action}`;
}

/** 删除签名载荷：`moments:delete:{postId}:{authorRootId}`——绑定「作者删了哪条动态」 */
function buildDeleteSignPayload(postId: string, authorRootId: string): string {
  return `moments:delete:${postId}:${authorRootId}`;
}

/** 互动广播的作者转发签名载荷（与 model.ts buildInteractionForwardSignPayload 逐字一致） */
function buildInteractionForwardSignPayload(
  postId: string,
  type: 'like' | 'comment',
  rootId: string,
  text: string,
  action: 'add' | 'remove'
): string {
  return `moments:interaction:forward:${postId}:${type}:${rootId}:${hashContent(text)}:${action}`;
}

/** 互动集合键：`{postId}:{type}:{rootId}` */
function interactionKey(postId: string, type: 'like' | 'comment', rootId: string): string {
  return `${postId}:${type}:${rootId}`;
}

/**
 * 互动通知去重键（与 model.ts interactionNotifyKey 逐字一致）：
 * `{postId}:{type}:{rootId}:{action}:{ts}`——同一互动事件的重复触发共键只通知一次，
 * 取消后重新互动是新事件（新 ts）照常通知。
 */
function interactionNotifyKey(postId: string, type: 'like' | 'comment', rootId: string, action: 'add' | 'remove', ts: number): string {
  return `${postId}:${type}:${rootId}:${action}:${ts}`;
}

/** 数组去重（保序） */
function dedupe<T>(list: T[]): T[] {
  return [...new Set(list)];
}

/** 互动广播名单：投递名单除互动发起者外（产品 §6.2 两跳链路第二跳） */
function computeInteractionBroadcast(postRecipients: string[], interactionRootId: string): string[] {
  return dedupe(postRecipients).filter((rootId) => rootId !== interactionRootId);
}

/** 动态正文摘要（截断 + 省略号；纯图片动态显示「[图片]」） */
function buildPostExcerpt(post: { text?: string }): string {
  const text = (post.text ?? '').trim();
  if (!text) return '[图片]';
  if (text.length <= EXCERPT_LENGTH) return text;
  return `${text.slice(0, EXCERPT_LENGTH)}…`;
}

/** 评论摘录（前 40 字） */
function buildCommentExcerpt(text: string): string {
  const normalized = text.trim();
  if (normalized.length <= COMMENT_EXCERPT_LENGTH) return normalized;
  return `${normalized.slice(0, COMMENT_EXCERPT_LENGTH)}…`;
}

/** 互动通知 summary（≤200 字符，未装插件时壳层原生渲染） */
function buildInteractionSummary(
  kind: 'like' | 'comment',
  fromName: string,
  count: number,
  postExcerpt: string,
  commentExcerpt?: string
): string {
  const who = count > 1 ? `${fromName} 等 ${count} 人` : fromName;
  const verb = kind === 'like' ? '赞了你的动态' : '评论了你的动态';
  if (kind === 'comment' && commentExcerpt && count === 1) {
    return `${who} ${verb}：${commentExcerpt}`.slice(0, SUMMARY_LIMIT);
  }
  return `${who} ${verb}：${postExcerpt}`.slice(0, SUMMARY_LIMIT);
}

// ---------------------------------------------------------------------------
// 集合声明（幂等）
// ---------------------------------------------------------------------------

function ensureCollections(): void {
  try {
    spark.data.declareCollection({ name: COLLECTIONS.posts });
    spark.data.declareCollection({ name: COLLECTIONS.interactions });
    // 通知去重台账：scope:'local'（不参与同步；U4，与视图侧 service.ts 同口径）
    spark.data.declareCollection({ name: COLLECTIONS.notified, scope: 'local' });
  } catch (err) {
    // 重复声明忽略（幂等）
    spark.log(`declare collection: ${String(err)}`);
  }
}

// ---------------------------------------------------------------------------
// 互动通知去重台账（U4 评审修复，对齐 spark-announcement/spark-kanban 台账范式）：
// 后台与 iframe 视图（service.notifyInteraction）是双写者，feed 重复投递 /
// onReceive 与 pull 补读双路径也会重复触发——台账以 sdk.data scope:'local'
// 持久面为准（与视图侧同一集合，跨写者去重）；QuickJS 无 localStorage，
// 持久面故障时退化为进程内 Set 会话级兜底。
// ---------------------------------------------------------------------------

const notifiedFallback = new Set<string>();

/** 查台账：持久面为准；读失败降级进程内兜底 */
function alreadyNotified(key: string): boolean {
  try {
    return !!spark.data.get(COLLECTIONS.notified, key);
  } catch (err) {
    spark.log(`[spark-moments][notify] ledger read error: ${String(err)}`);
    return notifiedFallback.has(key);
  }
}

/** 记台账：持久面为主，进程内兜底同步记录（写失败不阻塞通知流程） */
function markNotified(key: string, ts: number): void {
  notifiedFallback.add(key);
  try {
    spark.data.save(COLLECTIONS.notified, key, { ts });
  } catch (err) {
    spark.log(`[spark-moments][notify] ledger write error: ${String(err)}`);
  }
}

// ---------------------------------------------------------------------------
// 收件处理（post / interaction / delete，验签硬约束）
// ---------------------------------------------------------------------------

/** 验签动态：重算载荷比对 + 密码学验签。authorRootId 绑在载荷里，字段替换即失败。 */
function verifyPost(post: { id?: string; authorRootId?: string; text?: string; images?: Array<{ hash: string }>; signature?: { payload?: string; signature?: string; publicKey?: string } }): boolean {
  const sig = post?.signature;
  if (!sig || !sig.payload || !sig.signature || !sig.publicKey) {
    return false;
  }
  const expected = buildPostSignPayload(post as { id: string; authorRootId: string; text: string; images: Array<{ hash: string }> });
  if (sig.payload !== expected) {
    // 随帖载荷与当前内容/作者/归属不符：拒绝
    return false;
  }
  return spark.identity.verify({ payload: expected, sig: sig.signature, pubKey: sig.publicKey });
}

/** 验签互动：重算载荷比对 + 密码学验签。rootId 绑在载荷里，字段替换即失败。 */
function verifyInteraction(payload: { postId?: string; type?: 'like' | 'comment'; rootId?: string; interaction?: { text?: string; action?: 'add' | 'remove' }; signature?: { payload?: string; signature?: string; publicKey?: string } }): boolean {
  const sig = payload?.signature;
  if (!sig || !sig.payload || !sig.signature || !sig.publicKey || !payload?.interaction) {
    return false;
  }
  const expected = buildInteractionSignPayload(
    payload.postId ?? '',
    payload.type ?? 'like',
    payload.rootId ?? '',
    payload.interaction.text ?? '',
    payload.interaction.action ?? 'add'
  );
  if (sig.payload !== expected) {
    return false;
  }
  return spark.identity.verify({ payload: expected, sig: sig.signature, pubKey: sig.publicKey });
}

/** 验签作者转发签名（广播第二跳证据）：重算载荷比对 + 密码学验签。 */
function verifyInteractionForward(payload: { postId?: string; type?: 'like' | 'comment'; rootId?: string; interaction?: { text?: string; action?: 'add' | 'remove' }; forward?: { payload?: string; signature?: string; publicKey?: string } }): boolean {
  const fwd = payload?.forward;
  if (!fwd || !fwd.payload || !fwd.signature || !fwd.publicKey || !payload?.interaction) {
    return false;
  }
  const expected = buildInteractionForwardSignPayload(
    payload.postId ?? '',
    payload.type ?? 'like',
    payload.rootId ?? '',
    payload.interaction.text ?? '',
    payload.interaction.action ?? 'add'
  );
  if (fwd.payload !== expected) {
    return false;
  }
  return spark.identity.verify({ payload: expected, sig: fwd.signature, pubKey: fwd.publicKey });
}

/** 收动态（验签 → 落库；硬约束） */
function handlePost(msg: { payload: unknown; from: string }): void {
  const envelope = (msg.payload ?? {}) as { post?: unknown };
  const post = (envelope.post ?? envelope) as {
    id?: string;
    authorRootId?: string;
    text?: string;
    images?: Array<{ hash: string }>;
    recipients?: string[];
    signature?: { payload?: string; signature?: string; publicKey?: string };
  };
  if (!post || typeof post.id !== 'string' || typeof post.authorRootId !== 'string') {
    spark.log(`[spark-moments][post] drop: missing id/authorRootId`);
    return;
  }
  if (!verifyPost(post)) {
    console.warn(`[spark-moments][post] 拒收验签失败的动态 ${post.id} from=${msg.from}`);
    return;
  }
  spark.data.save(COLLECTIONS.posts, post.id, post);
  spark.log(`[spark-moments][post] accepted ${post.id} from=${msg.from}`);
}

/** 收互动（第一跳=作者收件带签名，验签 + 作者守卫后才广播；广播=双重签名证据验签后才落库） */
function handleInteraction(msg: { payload: unknown; from: string }): void {
  const payload = (msg.payload ?? {}) as {
    postId?: string;
    type?: 'like' | 'comment';
    rootId?: string;
    interaction?: { type?: 'like' | 'comment'; text?: string; action?: 'add' | 'remove'; ts?: number };
    broadcast?: boolean;
    signature?: { payload?: string; signature?: string; publicKey?: string };
    forward?: { payload?: string; signature?: string; publicKey?: string };
  };
  if (!payload.postId || !payload.type || !payload.rootId || !payload.interaction) {
    spark.log(`[spark-moments][interaction] drop: missing fields`);
    return;
  }

  // 广播型（非作者共同好友可见）：双重签名证据（原互动者签名 + 作者转发签名）
  // 验签硬约束 + 作者身份绑定（本地动态签名公钥须 == 转发公钥），任一失败即拒收；
  // 只落库不广播。
  if (payload.broadcast === true) {
    if (!verifyInteraction(payload) || !verifyInteractionForward(payload)) {
      console.warn(`[spark-moments][interaction] 拒收验签失败的互动广播 ${payload.postId} from=${msg.from}`);
      return;
    }
    const localPost = spark.data.get(COLLECTIONS.posts, payload.postId) as {
      signature?: { publicKey?: string };
    } | null;
    if (!localPost || !localPost.signature?.publicKey || localPost.signature.publicKey !== payload.forward?.publicKey) {
      console.warn(`[spark-moments][interaction] 拒收作者身份无法绑定的互动广播 ${payload.postId} from=${msg.from}`);
      return;
    }
    spark.data.save(
      COLLECTIONS.interactions,
      interactionKey(payload.postId, payload.type, payload.rootId),
      payload.interaction
    );
    spark.log(`[spark-moments][interaction] broadcast applied ${payload.postId}:${payload.type}:${payload.rootId}`);
    return;
  }

  // 第一跳（互动者 → 作者）：验签硬约束，失败绝不落库
  if (!verifyInteraction(payload)) {
    console.warn(`[spark-moments][interaction] 拒收验签失败的互动 ${payload.postId} from=${msg.from}`);
    return;
  }
  spark.data.save(
    COLLECTIONS.interactions,
    interactionKey(payload.postId, payload.type, payload.rootId),
    payload.interaction
  );

  // 第一跳 recipients 由发送方指定，内核只做形态/收件人过滤——「内核定向投递到作者
  // 故本机即作者」的假设不成立（攻击者可把第一跳直接投递给非作者受害机）。作者守卫：
  // 以本机域身份签转发载荷，比较签名公钥与本地动态签名公钥——相等即本机是作者设备
  // （域身份签名无法反推 rootId，公钥比对是沙箱内可用的作者判定）。
  const post = spark.data.get(COLLECTIONS.posts, payload.postId) as {
    id?: string;
    authorRootId?: string;
    text?: string;
    images?: Array<{ hash: string }>;
    recipients?: string[];
    authorSnapshot?: { nickname?: string; avatar?: string | null };
    signature?: { payload?: string; signature?: string; publicKey?: string };
  } | null;
  if (!post) {
    spark.log(`[spark-moments][interaction] post not found locally ${payload.postId}; skip broadcast/notify`);
    return;
  }

  const forwardPayload = buildInteractionForwardSignPayload(
    payload.postId,
    payload.type,
    payload.rootId,
    payload.interaction.text ?? '',
    payload.interaction.action ?? 'add'
  );
  let forward: { payload: string; signature: string; publicKey: string } | null = null;
  try {
    const signed = spark.identity.sign(forwardPayload);
    forward = { payload: forwardPayload, signature: signed.signature, publicKey: signed.publicKey };
  } catch (err) {
    spark.log(`[spark-moments][interaction] forward sign error: ${String(err)}`);
  }
  if (!forward || !post.signature?.publicKey || post.signature.publicKey !== forward.publicKey) {
    // 本机不是作者设备（或动态无签名/签名被拒）：只落库，不广播不通知（防伪造放大）
    spark.log(`[spark-moments][interaction] not author device for ${payload.postId}; skip broadcast/notify`);
    return;
  }

  // 广播名单（除互动发起者；与 service.broadcastInteraction 同口径）：
  // 携带原互动者签名 + 作者转发签名双重证据，接收方验签后才落库
  const recipients = computeInteractionBroadcast(post.recipients ?? [], payload.rootId);
  if (recipients.length > 0) {
    try {
      spark.feed.deliver({
        topic: TOPICS.interaction,
        payload: { postId: payload.postId, type: payload.type, rootId: payload.rootId, interaction: payload.interaction, broadcast: true, signature: payload.signature, forward },
        recipients,
        replyTo: payload.postId
      });
    } catch (err) {
      spark.log(`[spark-moments][interaction] broadcast error: ${String(err)}`);
    }
  }

  // 应用会话通知（后台无通讯录，互动者昵称降级用 rootId；message:app 限流降级）
  notifyAuthor(payload, post);
}

/** 写互动通知应用消息（summary + notify-card 卡片；权限/限流降级 try/catch；台账去重） */
function notifyAuthor(
  payload: { postId?: string; type?: 'like' | 'comment'; rootId?: string; interaction?: { text?: string; action?: 'add' | 'remove'; ts?: number } },
  post: { text?: string; images?: Array<{ hash: string }> }
): void {
  const kind = payload.type ?? 'like';
  const fromRootId = payload.rootId ?? '';
  const fromName = fromRootId || '(未知)';
  const interaction = payload.interaction ?? {};
  // 去重（U4）：同一互动事件视图侧/后台重复触发只通知一次（台账为持久面，
  // 先于发送判定，发送成功后记账——与 service.notifyInteraction 同口径）
  const dedupKey = interactionNotifyKey(payload.postId ?? '', kind, fromRootId, interaction.action ?? 'add', interaction.ts ?? 0);
  if (alreadyNotified(dedupKey)) {
    spark.log(`[spark-moments][notify] dedup skip ${dedupKey}`);
    return;
  }
  const postExcerpt = buildPostExcerpt(post);
  const commentExcerpt = kind === 'comment' ? buildCommentExcerpt(interaction.text ?? '') : undefined;
  const summary = buildInteractionSummary(kind, fromName, 1, postExcerpt, commentExcerpt);
  try {
    spark.messages.sendAppMessage({
      summary,
      card: {
        viewId: 'notify-card',
        data: {
          kind,
          fromRootIds: [fromRootId],
          fromName,
          fromAvatar: null,
          count: 1,
          postId: payload.postId ?? '',
          postExcerpt,
          postThumbHash: post.images?.[0]?.hash ?? null,
          commentExcerpt,
          ts: interaction.ts ?? Date.now()
        }
      }
    });
    markNotified(dedupKey, interaction.ts ?? Date.now());
  } catch (err) {
    spark.log(`[spark-moments][notify] app message failed: ${String(err)}`);
  }
}

/** 收删除通知（作者删 → 本地标记 deletedAt）。签名验签 + 作者一致性硬约束；无签名一律拒收。 */
function handleDelete(msg: { payload: unknown; from: string }): void {
  const payload = (msg.payload ?? {}) as {
    postId?: string;
    authorRootId?: string;
    deletedAt?: number;
    sig?: string;
    pubKey?: string;
    signature?: { payload?: string; signature?: string; publicKey?: string };
  };
  if (!payload.postId || !payload.authorRootId) {
    spark.log(`[spark-moments][delete] drop: missing postId/authorRootId`);
    return;
  }

  // 验签硬约束：删除通知必须带作者签名（无签名的旧版删除一律拒收——
  // 跨版本兼容以「旧版删除不生效 + 日志」为代价换安全）。
  // 兼容两种签名形态：signature 对象（service.deletePost 线形）或 sig/pubKey 扁平字段
  // （模型 MomentsDeletePayload 形态）。
  const sig = payload.signature
    ? payload.signature
    : payload.sig && payload.pubKey
      ? { payload: buildDeleteSignPayload(payload.postId, payload.authorRootId), signature: payload.sig, publicKey: payload.pubKey }
      : undefined;
  if (!sig || !sig.payload || !sig.signature || !sig.publicKey) {
    console.warn(`[spark-moments][delete] 拒收无签名的删除 ${payload.postId} from=${msg.from}`);
    return;
  }
  const expected = buildDeleteSignPayload(payload.postId, payload.authorRootId);
  if (sig.payload !== expected) {
    console.warn(`[spark-moments][delete] 拒收验签失败的删除 ${payload.postId} from=${msg.from}`);
    return;
  }
  if (!spark.identity.verify({ payload: expected, sig: sig.signature, pubKey: sig.publicKey })) {
    console.warn(`[spark-moments][delete] 拒收验签失败的删除 ${payload.postId} from=${msg.from}`);
    return;
  }

  const post = spark.data.get(COLLECTIONS.posts, payload.postId) as Record<string, unknown> | null;
  if (!post) return;
  // 作者一致性：payload.authorRootId 须等于本地动态作者；动态有签名时其公钥
  // 须与删除签名公钥一致（作者身份绑定，防伪造者冒签他人 postId）
  if (post.authorRootId !== payload.authorRootId) {
    console.warn(`[spark-moments][delete] 拒收作者不符的删除 ${payload.postId} from=${msg.from}`);
    return;
  }
  const postSig = post.signature as { publicKey?: string } | undefined;
  if (postSig?.publicKey && postSig.publicKey !== sig.publicKey) {
    console.warn(`[spark-moments][delete] 拒收签名公钥与动态作者不符的删除 ${payload.postId} from=${msg.from}`);
    return;
  }
  spark.data.save(COLLECTIONS.posts, payload.postId, { ...post, deletedAt: payload.deletedAt ?? Date.now() });
  spark.log(`[spark-moments][delete] marked deletedAt ${payload.postId}`);
}

// ---------------------------------------------------------------------------
// 订阅 + 启动补读（onReceive 与 pull 复用同一处理路径）
// ---------------------------------------------------------------------------

/** 按 topic 分发的统一处理入口（onReceive 与 pull 补读共用） */
function dispatch(msg: { topic: string; payload: unknown; from: string; ts: number }): void {
  if (msg.topic === TOPICS.post) {
    receivedCounts.post += 1;
    handlePost(msg);
  } else if (msg.topic === TOPICS.interaction) {
    receivedCounts.interaction += 1;
    handleInteraction(msg);
  } else if (msg.topic === TOPICS.delete) {
    receivedCounts.delete += 1;
    handleDelete(msg);
  }
}

/** 订阅 feed 收件（topic 前缀匹配；处理复用 dispatch） */
function subscribeInbox(): void {
  spark.feed.onReceive(TOPICS.post, (msg) => {
    spark.log(`feed post received#${receivedCounts.post + 1} from=${msg.from}`);
    dispatch({ ...msg, topic: TOPICS.post });
  });
  spark.feed.onReceive(TOPICS.interaction, (msg) => {
    spark.log(`feed interaction received#${receivedCounts.interaction + 1} from=${msg.from}`);
    dispatch({ ...msg, topic: TOPICS.interaction });
  });
  spark.feed.onReceive(TOPICS.delete, (msg) => {
    spark.log(`feed delete received#${receivedCounts.delete + 1} from=${msg.from}`);
    dispatch({ ...msg, topic: TOPICS.delete });
  });
}

/** 启动补读收件箱（崩溃/未运行期间进入收件箱的数据）；处理复用 dispatch */
function pullInbox(): void {
  const topics = [TOPICS.post, TOPICS.interaction, TOPICS.delete];
  for (const topic of topics) {
    try {
      let cursor: string | undefined;
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const res = spark.feed.pull({ topic, cursor, limit: 50 });
        for (const item of res.items ?? []) {
          dispatch({ ...item, topic });
        }
        if (!res.nextCursor) break;
        cursor = res.nextCursor;
      }
    } catch (err) {
      spark.log(`feed pull error ${topic}: ${String(err)}`);
    }
  }
}

ensureCollections();
subscribeInbox();
pullInbox();
spark.log(`spark-moments background started (post/interaction/delete verified ingest + interaction notify)`);
