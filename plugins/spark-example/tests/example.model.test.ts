import { describe, expect, it } from 'vitest';
import {
  WEIBO_MAX_TEXT_LENGTH,
  buildCommentThread,
  buildPostSignPayload,
  buildPostSummary,
  canPublishPost,
  hashPostContent,
  validateWeiboText,
  type WeiboComment
} from '../model';

describe('spark-example model', () => {
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

  it('hashes post content deterministically (stable sign payload material)', () => {
    expect(hashPostContent('hello spark')).toBe(hashPostContent('hello spark'));
    expect(hashPostContent('hello spark')).not.toBe(hashPostContent('hello sparx'));
    expect(hashPostContent('')).toMatch(/^[0-9a-f]{8}$/);
  });

  it('binds sign payload to org + post + author + content hash (anti replay, anti author-swap)', () => {
    const payload = buildPostSignPayload('org-1', 'post-1', 'root-admin', '正文');
    expect(payload).toBe(`org-1:post-1:root-admin:${hashPostContent('正文')}`);
    // 同一正文换个帖子/组织/作者，载荷不同——签名无法被剪贴重放、作者无法被替换
    expect(buildPostSignPayload('org-1', 'post-2', 'root-admin', '正文')).not.toBe(payload);
    expect(buildPostSignPayload('org-2', 'post-1', 'root-admin', '正文')).not.toBe(payload);
    expect(buildPostSignPayload('org-1', 'post-1', 'root-other', '正文')).not.toBe(payload);
  });

  it('builds self-contained app message summary (declarative fallback text)', () => {
    expect(buildPostSummary('  今天发布了新版本  ')).toBe('【新帖】今天发布了新版本');
    // 摘要必须 ≤200 字符（内核硬约束）：长正文截断并加省略号
    const long = '长'.repeat(300);
    const summary = buildPostSummary(long);
    expect(summary.length).toBeLessThanOrEqual(200);
    expect(summary.endsWith('…')).toBe(true);
    expect(summary.startsWith('【新帖】')).toBe(true);
  });
});
