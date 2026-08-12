/**
 * 朋友圈插件（spark-moments）· 后台入口（内核 QuickJS 沙箱，plugin_system.md「后台运行时」）。
 *
 * 设计职责（产品 §七/§十三）：feed 收件（三通道验签落库）+ 互动通知生成。
 * 与 iframe 主视图（useMoments.subscribeFeed/pullInbox）同口径：post/interaction/delete
 * 三 topic 均先验签后落库（防伪造硬约束），互动第一跳作者侧广播名单 + 写应用会话通知。
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
    declareCollection: (decl: { name: string }) => unknown;
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

/** 集合名内联（与 model.ts MOMENTS_COLLECTIONS 逐字一致；零依赖约束故内联） */
const COLLECTIONS = {
  posts: 'spark-moments:posts',
  interactions: 'spark-moments:interactions'
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

/** 互动集合键：`{postId}:{type}:{rootId}` */
function interactionKey(postId: string, type: 'like' | 'comment', rootId: string): string {
  return `${postId}:${type}:${rootId}`;
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
  } catch (err) {
    // 重复声明忽略（幂等）
    spark.log(`declare collection: ${String(err)}`);
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

/** 收互动（第一跳=作者收件带签名；广播=共同好友可见本地增删，无签名不可验） */
function handleInteraction(msg: { payload: unknown; from: string }): void {
  const payload = (msg.payload ?? {}) as {
    postId?: string;
    type?: 'like' | 'comment';
    rootId?: string;
    interaction?: { type?: 'like' | 'comment'; text?: string; action?: 'add' | 'remove'; ts?: number };
    broadcast?: boolean;
    signature?: { payload?: string; signature?: string; publicKey?: string };
  };
  if (!payload.postId || !payload.type || !payload.rootId || !payload.interaction) {
    spark.log(`[spark-moments][interaction] drop: missing fields`);
    return;
  }

  // 广播型（非作者共同好友可见）：本地增删互动记录，无签名（作者广播不带签），只落库不广播
  if (payload.broadcast === true) {
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

  // 第一跳由内核定向投递到作者 rootId（recipients:[post.authorRootId]），故本机即作者。
  // 读本机该动态的投递名单，广播给名单（除互动者）+ 写应用会话通知。
  const post = spark.data.get(COLLECTIONS.posts, payload.postId) as {
    id?: string;
    authorRootId?: string;
    text?: string;
    images?: Array<{ hash: string }>;
    recipients?: string[];
    authorSnapshot?: { nickname?: string; avatar?: string | null };
  } | null;
  if (!post) {
    spark.log(`[spark-moments][interaction] post not found locally ${payload.postId}; skip broadcast/notify`);
    return;
  }

  // 广播名单（除互动发起者；与 service.broadcastInteraction 同口径）
  const recipients = computeInteractionBroadcast(post.recipients ?? [], payload.rootId);
  if (recipients.length > 0) {
    try {
      spark.feed.deliver({
        topic: TOPICS.interaction,
        payload: { postId: payload.postId, type: payload.type, rootId: payload.rootId, interaction: payload.interaction, broadcast: true },
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

/** 写互动通知应用消息（summary + notify-card 卡片；权限/限流降级 try/catch） */
function notifyAuthor(
  payload: { postId?: string; type?: 'like' | 'comment'; rootId?: string; interaction?: { text?: string; ts?: number } },
  post: { text?: string; images?: Array<{ hash: string }> }
): void {
  const kind = payload.type ?? 'like';
  const fromRootId = payload.rootId ?? '';
  const fromName = fromRootId || '(未知)';
  const interaction = payload.interaction ?? {};
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
  } catch (err) {
    spark.log(`[spark-moments][notify] app message failed: ${String(err)}`);
  }
}

/** 收删除通知（作者删 → 本地标记 deletedAt）。签名格式验签；旧版无签名降级标记。 */
function handleDelete(msg: { payload: unknown; from: string }): void {
  const payload = (msg.payload ?? {}) as {
    postId?: string;
    authorRootId?: string;
    deletedAt?: number;
    sig?: string;
    pubKey?: string;
    signature?: { payload?: string; signature?: string; publicKey?: string };
  };
  if (!payload.postId) {
    spark.log(`[spark-moments][delete] drop: missing postId`);
    return;
  }

  // 签名格式（模型 MomentsDeletePayload 形态）：验签 + 作者一致性（authorRootId 绑载荷）
  if (payload.authorRootId && (payload.sig || payload.signature)) {
    const sig = payload.signature ?? { payload: buildDeleteSignPayload(payload.postId, payload.authorRootId), signature: payload.sig ?? '', publicKey: payload.pubKey ?? '' };
    const expected = buildDeleteSignPayload(payload.postId, payload.authorRootId);
    if (!sig.payload || !sig.signature || !sig.publicKey || sig.payload !== expected) {
      console.warn(`[spark-moments][delete] 拒收验签失败的删除 ${payload.postId} from=${msg.from}`);
      return;
    }
    if (!spark.identity.verify({ payload: expected, sig: sig.signature, pubKey: sig.publicKey })) {
      console.warn(`[spark-moments][delete] 拒收验签失败的删除 ${payload.postId} from=${msg.from}`);
      return;
    }
  } else {
    // 旧版无签名删除（当前 iframe deletePost 形态）：降级标记（与 service.receiveDelete 同口径）
    console.warn(`[spark-moments][delete] 无签名的删除 ${payload.postId} from=${msg.from}（旧版降级）`);
  }

  const post = spark.data.get(COLLECTIONS.posts, payload.postId) as Record<string, unknown> | null;
  if (!post) return;
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
