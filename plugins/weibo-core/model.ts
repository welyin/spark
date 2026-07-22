export const WEIBO_MAX_TEXT_LENGTH = 260;

export type WeiboPost = {
  id: string;
  orgId: string;
  content: string;
  authorRootId: string;
  createdAt: number;
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
