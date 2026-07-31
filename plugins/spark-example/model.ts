/**
 * 示例插件（spark-example）· 数据模型与纯函数。
 *
 * 教学要点：
 * - 本文件不依赖 SDK / Vue，全部是可单测的纯函数与类型——插件业务规则
 *   （长度约束、发帖权限、评论树组装、签名载荷、消息摘要）尽量沉淀在这一层；
 * - 集合名沿用 weibo_* 旧称只是减少无谓 churn，**不等于存量数据兼容**：
 *   内核存储键含插件域段（`doc:<domain>:<collection>:<id>`），域已从
 *   weibo-core 更名为 spark-example，全部存储键随之改变——本次更名是
 *   「不兼容升级」，旧 weibo-core 域的存量数据不迁移（0.1.0 预发布阶段
 *   显式接受；正式版本若再更名/改域，必须提供迁移或兼容层）；
 * - 真正的跨版本兼容手段在文档结构层：WeiboPost.signature 为可选新增字段，
 *   旧帖子文档没有它也能正常读写——插件演进数据结构的安全方式是
 *   只加可选字段，不改/不删既有字段。
 */

export const WEIBO_MAX_TEXT_LENGTH = 260;

/** 应用消息摘要中正文预览的最大字数（summary 上限 200 字符，留足前缀余量） */
export const POST_SUMMARY_PREVIEW_LENGTH = 80;

/**
 * 帖子签名信息（identity:sign 防抵赖演示）。
 * 随帖存储在 WeiboPost.signature：签名出自插件域身份（域私钥永不离开内核），
 * 任何成员拿到 payload + signature + publicKey 都可用 identity.verify 免权限验签。
 */
export type WeiboPostSignature = {
  /** 被签名的原文（buildPostSignPayload 产物）；验签侧不直接回放，而是从帖子当前字段重算比对 */
  payload: string;
  signature: string;
  publicKey: string;
};

export type WeiboPost = {
  id: string;
  orgId: string;
  content: string;
  authorRootId: string;
  createdAt: number;
  /** 可选：发帖时的域身份签名（用户拒绝授权或旧版本帖子则无此字段） */
  signature?: WeiboPostSignature;
};

export type WeiboComment = {
  id: string;
  orgId: string;
  postId: string;
  parentCommentId?: string;
  content: string;
  authorRootId: string;
  createdAt: number;
};

export type WeiboCommentNode = {
  comment: WeiboComment;
  replies: WeiboComment[];
};

export function canPublishPost(currentRole: 'admin' | 'member' | null | undefined): boolean {
  return currentRole === 'admin';
}

export function normalizeWeiboText(content: string): string {
  return content.trim();
}

export function validateWeiboText(content: string): { ok: boolean; reason?: string } {
  const normalized = normalizeWeiboText(content);
  if (!normalized) {
    return { ok: false, reason: '内容不能为空' };
  }
  if (normalized.length > WEIBO_MAX_TEXT_LENGTH) {
    return { ok: false, reason: `内容长度不能超过${WEIBO_MAX_TEXT_LENGTH}字` };
  }
  return { ok: true };
}

/**
 * 内容哈希（FNV-1a 32bit，hex 输出）。
 * 教学说明：这里只需要一个稳定、确定性的内容指纹来压缩签名载荷长度，
 * 防抵赖强度由身份模块的 Ed25519 域签名保证，不依赖本哈希的抗碰撞性；
 * 插件沙箱内不假设 WebCrypto 可用（opaque origin iframe），故用纯 TS 实现。
 */
export function hashPostContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    // 乘以 FNV 素数 16777619（用位运算避免浮点）
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/**
 * 签名载荷：`{orgId}:{postId}:{authorRootId}:{内容哈希}`。
 * 把组织、帖子 id、作者与内容指纹全部编进载荷：签名即绑定
 * 「谁在哪个组织以哪个身份发了哪条帖」，无法被剪贴到别的帖子/组织上
 * 重放，也无法在保留签名的前提下替换作者字段（验签侧会用帖子当前
 * authorRootId 重算载荷比对，见 service.verifyPostSignature）。
 */
export function buildPostSignPayload(orgId: string, postId: string, authorRootId: string, content: string): string {
  return `${orgId}:${postId}:${authorRootId}:${hashPostContent(content)}`;
}

/**
 * 应用消息摘要（声明式降级文本，p2p-messages.md §20：summary 强制）。
 * 未安装插件的成员设备上，壳层原生渲染这段纯文本，因此摘要必须自成一体、
 * 不依赖卡片数据也能读懂——可达性不依赖成员是否安装插件代码。
 */
export function buildPostSummary(content: string): string {
  const normalized = normalizeWeiboText(content);
  const preview = normalized.slice(0, POST_SUMMARY_PREVIEW_LENGTH);
  const ellipsis = normalized.length > POST_SUMMARY_PREVIEW_LENGTH ? '…' : '';
  return `【新帖】${preview}${ellipsis}`;
}

export function buildCommentThread(postId: string, comments: WeiboComment[]): WeiboCommentNode[] {
  const forPost = comments
    .filter((item) => item.postId === postId)
    .sort((a, b) => a.createdAt - b.createdAt);

  const roots = forPost.filter((item) => !item.parentCommentId);
  const repliesByParent = new Map<string, WeiboComment[]>();

  for (const comment of forPost) {
    if (!comment.parentCommentId) {
      continue;
    }

    const bucket = repliesByParent.get(comment.parentCommentId) ?? [];
    bucket.push(comment);
    repliesByParent.set(comment.parentCommentId, bucket);
  }

  return roots.map((root) => ({
    comment: root,
    replies: (repliesByParent.get(root.id) ?? []).sort((a, b) => a.createdAt - b.createdAt)
  }));
}
