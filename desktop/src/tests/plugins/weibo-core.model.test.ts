import { describe, expect, it } from 'vitest';
import {
  WEIBO_MAX_TEXT_LENGTH,
  buildCommentThread,
  canPublishPost,
  validateWeiboText,
  type WeiboComment
} from '../../../../plugins/weibo-core/model';

describe('weibo-core model', () => {
  it('enforces publish permission to organization admins only', () => {
    expect(canPublishPost('admin')).toBe(true);
    expect(canPublishPost('member')).toBe(false);
    expect(canPublishPost(null)).toBe(false);
  });

  it('enforces 260-char max text constraint', () => {
    const valid = 'a'.repeat(WEIBO_MAX_TEXT_LENGTH);
    const invalid = 'a'.repeat(WEIBO_MAX_TEXT_LENGTH + 1);

    expect(validateWeiboText(valid).ok).toBe(true);
    expect(validateWeiboText(invalid).ok).toBe(false);
    expect(validateWeiboText('    ').ok).toBe(false);
  });

  it('builds comment-reply structure in chronological order', () => {
    const comments: WeiboComment[] = [
      {
        id: 'c2',
        orgId: 'org-1',
        postId: 'p1',
        content: 'reply',
        authorRootId: 'u2',
        parentCommentId: 'c1',
        createdAt: 3
      },
      {
        id: 'c1',
        orgId: 'org-1',
        postId: 'p1',
        content: 'root',
        authorRootId: 'u1',
        createdAt: 1
      },
      {
        id: 'c3',
        orgId: 'org-1',
        postId: 'p1',
        content: 'root-2',
        authorRootId: 'u3',
        createdAt: 2
      }
    ];

    const thread = buildCommentThread('p1', comments);
    expect(thread).toHaveLength(2);
    expect(thread[0].comment.id).toBe('c1');
    expect(thread[0].replies).toHaveLength(1);
    expect(thread[0].replies[0].id).toBe('c2');
    expect(thread[1].comment.id).toBe('c3');
  });
});
