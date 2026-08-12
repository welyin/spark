import { describe, expect, it } from 'vitest';
import {
  MOMENTS_MAX_TEXT_LENGTH,
  MOMENTS_MAX_COMMENT_LENGTH,
  MOMENTS_MAX_IMAGES,
  buildDeleteSignPayload,
  buildInteractionSignPayload,
  buildMomentId,
  buildPostSignPayload,
  computeDeleteBroadcast,
  computeInteractionBroadcast,
  decodeDeletePayload,
  decodeInteractionPayload,
  decodePostPayload,
  dedupe,
  encodeDeletePayload,
  encodeInteractionPayload,
  encodePostPayload,
  expandRecipients,
  expandRecipientsFromSelection,
  formatRelativeTime,
  hashContent,
  interactionKey,
  sortTimeline,
  validateCommentText,
  validateImages,
  validateMomentsText,
  type MomentsImage,
  type MomentsPost
} from '../model';

function mkPost(overrides: Partial<MomentsPost> = {}): MomentsPost {
  return {
    id: 'post-1',
    authorRootId: 'root-a',
    text: '正文',
    images: [],
    createdAt: 100,
    visibleScope: 'all',
    visibleList: ['root-b'],
    recipients: ['root-b'],
    ...overrides
  };
}

describe('spark-moments model', () => {
  it('enforces text length constraints', () => {
    expect(validateMomentsText('a'.repeat(MOMENTS_MAX_TEXT_LENGTH)).ok).toBe(true);
    expect(validateMomentsText('a'.repeat(MOMENTS_MAX_TEXT_LENGTH + 1)).ok).toBe(false);
    expect(validateMomentsText('   ').ok).toBe(false);
    expect(validateCommentText('a'.repeat(MOMENTS_MAX_COMMENT_LENGTH)).ok).toBe(true);
    expect(validateCommentText('a'.repeat(MOMENTS_MAX_COMMENT_LENGTH + 1)).ok).toBe(false);
  });

  // ------------------------------------------------------------------
  // 可见性四选一展开（产品 §5.3，发送方裁决）
  // ------------------------------------------------------------------

  it('expands public to all contacts', () => {
    const { recipients, visibleList } = expandRecipients('all', ['root-b', 'root-c'], []);
    expect(recipients).toEqual(['root-b', 'root-c']);
    expect(visibleList).toEqual(['root-b', 'root-c']);
  });

  it('private expands to empty (no delivery, local only)', () => {
    const { recipients, visibleList } = expandRecipients('private', ['root-b', 'root-c'], ['root-b']);
    expect(recipients).toEqual([]);
    expect(visibleList).toEqual([]);
  });

  it('partial keeps only selected (deduped)', () => {
    const { recipients, visibleList } = expandRecipients('partial', ['root-b', 'root-c'], ['root-b', 'root-c', 'root-b']);
    expect(recipients).toEqual(['root-b', 'root-c']);
    expect(visibleList).toEqual(['root-b', 'root-c']);
  });

  it('exclude removes selected from all contacts', () => {
    const { recipients } = expandRecipients('exclude', ['root-b', 'root-c', 'root-d'], ['root-c']);
    expect(recipients).toEqual(['root-b', 'root-d']);
  });

  it('exclude with empty selection delivers to all (no-op exclusion)', () => {
    const { recipients } = expandRecipients('exclude', ['root-b', 'root-c'], []);
    expect(recipients).toEqual(['root-b', 'root-c']);
  });

  // ------------------------------------------------------------------
  // 互动广播名单计算（产品 §6.2 两跳链路第二跳）
  // ------------------------------------------------------------------

  it('broadcasts interaction to post recipients except the interaction author', () => {
    const recipients = ['root-b', 'root-c', 'root-d'];
    expect(computeInteractionBroadcast(recipients, 'root-b')).toEqual(['root-c', 'root-d']);
    // 发起者不在名单内：全员收到
    expect(computeInteractionBroadcast(recipients, 'root-x')).toEqual(['root-b', 'root-c', 'root-d']);
    // 去重
    expect(computeInteractionBroadcast(['b', 'b', 'c'], 'b')).toEqual(['c']);
  });

  // ------------------------------------------------------------------
  // 删除级联广播（产品 §6.5）
  // ------------------------------------------------------------------

  it('delete broadcast targets original recipients', () => {
    expect(computeDeleteBroadcast(['root-b', 'root-c'])).toEqual(['root-b', 'root-c']);
    expect(computeDeleteBroadcast([])).toEqual([]);
    expect(computeDeleteBroadcast(['b', 'b', 'c'])).toEqual(['b', 'c']);
  });

  // ------------------------------------------------------------------
  // 签名载荷（防剪贴重放、防作者替换）
  // ------------------------------------------------------------------

  it('binds post sign payload to id + author + content hash + image hashes', () => {
    const post = mkPost();
    const p1 = buildPostSignPayload(post);
    const swappedAuthor = buildPostSignPayload(mkPost({ authorRootId: 'root-x' }));
    const swappedText = buildPostSignPayload(mkPost({ text: '别的正文' }));
    expect(p1).not.toBe(swappedAuthor);
    expect(p1).not.toBe(swappedText);
    // 图片哈希列表参与绑定
    const withImages = buildPostSignPayload(
      mkPost({ images: [{ hash: 'h1', thumbHash: 't1', name: 'a.jpg', size: 1, mime: 'image/jpeg' }] })
    );
    expect(withImages).not.toBe(p1);
  });

  it('binds interaction sign payload to postId + type + rootId + text + action', () => {
    const add = buildInteractionSignPayload('p1', 'comment', 'root-b', '真好看', 'add');
    const remove = buildInteractionSignPayload('p1', 'comment', 'root-b', '真好看', 'remove');
    const like = buildInteractionSignPayload('p1', 'like', 'root-b', '', 'add');
    expect(add).not.toBe(remove);
    expect(add).not.toBe(like);
    expect(buildInteractionSignPayload('p2', 'comment', 'root-b', '真好看', 'add')).not.toBe(add);
  });

  // ------------------------------------------------------------------
  // 互动 key / 去重 / 排序 / 时间
  // ------------------------------------------------------------------

  it('builds composite interaction key', () => {
    expect(interactionKey('post-1', 'like', 'root-b')).toBe('post-1:like:root-b');
    expect(interactionKey('post-1', 'comment', 'root-b')).toBe('post-1:comment:root-b');
  });

  it('dedupes preserving order', () => {
    expect(dedupe(['a', 'b', 'a', 'c'])).toEqual(['a', 'b', 'c']);
  });

  it('sorts timeline newest first and filters deleted', () => {
    const items = [
      mkPost({ id: 'p1', createdAt: 100 }),
      mkPost({ id: 'p2', createdAt: 300, deletedAt: 500 }),
      mkPost({ id: 'p3', createdAt: 200 })
    ];
    expect(sortTimeline(items).map((p) => p.id)).toEqual(['p3', 'p1']);
  });

  it('formats relative time', () => {
    const now = 1_000_000;
    expect(formatRelativeTime(now - 5_000, now)).toBe('刚刚');
    expect(formatRelativeTime(now - 60_000, now)).toBe('1 分钟前');
  });

  it('hashes content deterministically', () => {
    expect(hashContent('hello')).toBe(hashContent('hello'));
    expect(hashContent('hello')).not.toBe(hashContent('hellp'));
  });
});

describe('spark-moments model · 追加层（id/校验/勾选项展开/payload 编解码）', () => {
  // ------------------------------------------------------------------
  // id 生成
  // ------------------------------------------------------------------

  it('builds moment id with author prefix + ts + rand (unique & format)', () => {
    const id = buildMomentId('root-author-12345678', 1000, 'abcdef');
    // authorRootId 前 8 位为 'root-aut'
    expect(id).toBe('moment-root-aut-1000-abcdef');
    expect(id).toMatch(/^moment-[0-9a-z-]{8}-\d+-[0-9a-f]{6,}$/);
    // 同一作者同一 ts 不同 rand 不碰撞
    expect(buildMomentId('r1', 5, 'aaaa')).not.toBe(buildMomentId('r1', 5, 'bbbb'));
    // 不同 ts 不同 id
    expect(buildMomentId('r1', 5, 'aaaa')).not.toBe(buildMomentId('r1', 6, 'aaaa'));
  });

  // ------------------------------------------------------------------
  // 校验
  // ------------------------------------------------------------------

  it('validates image count (1–9) and per-image size', () => {
    const img = (i: number, size = 1024): MomentsImage => ({
      hash: `h${i}`,
      thumbHash: `t${i}`,
      name: `${i}.jpg`,
      size,
      mime: 'image/jpeg'
    });
    expect(validateImages([img(1)]).ok).toBe(true);
    expect(validateImages(Array.from({ length: MOMENTS_MAX_IMAGES }, (_, i) => img(i))).ok).toBe(true);
    expect(validateImages([]).ok).toBe(false); // 至少 1 张
    expect(validateImages(Array.from({ length: MOMENTS_MAX_IMAGES + 1 }, (_, i) => img(i))).ok).toBe(false);
    expect(validateImages([img(1, 10 * 1024 * 1024 + 1)]).ok).toBe(false); // 超 10MB
  });

  // ------------------------------------------------------------------
  // 删除签名载荷
  // ------------------------------------------------------------------

  it('builds delete sign payload binding postId + authorRootId', () => {
    const p = buildDeleteSignPayload('post-1', 'root-a');
    expect(p).toBe('moments:delete:post-1:root-a');
    expect(buildDeleteSignPayload('post-2', 'root-a')).not.toBe(p);
    expect(buildDeleteSignPayload('post-1', 'root-b')).not.toBe(p);
  });

  // ------------------------------------------------------------------
  // 勾选项展开（联系人/分组/标签混合 + 四选一）
  // ------------------------------------------------------------------

  const friends = [
    { rootId: 'f1', groupId: 'g1', tagIds: ['t1'] },
    { rootId: 'f2', groupId: 'g1', tagIds: [] },
    { rootId: 'f3', groupId: 'g2', tagIds: ['t2'] },
    { rootId: 'f4', groupId: '', tagIds: ['t1'] },
    { rootId: 'f5', groupId: 'g2', tagIds: [] }
  ];
  const allSelected = () => ({ contactRootIds: ['f1'], groupIds: ['g2'], tagIds: ['t1'] });

  it('expands contact + group + tag selections into deduped rootId set', () => {
    const { selectedRootIds } = expandRecipientsFromSelection('partial', allSelected(), friends);
    // 遍历 friends 顺序：f1(直选) → f3(分组g2) → f4(标签t1) → f5(分组g2)，去重后保持顺序
    expect(selectedRootIds).toEqual(['f1', 'f3', 'f4', 'f5']);
  });

  it('expands partial via four-tier decision after selection expansion', () => {
    const { allRootIds, selectedRootIds } = expandRecipientsFromSelection('partial', allSelected(), friends);
    const { recipients } = expandRecipients('partial', allRootIds, selectedRootIds);
    expect(recipients).toEqual(['f1', 'f3', 'f4', 'f5']);
  });

  it('exclude removes group/tag-expanded members from all contacts', () => {
    const { allRootIds, selectedRootIds } = expandRecipientsFromSelection('exclude', allSelected(), friends);
    const { recipients } = expandRecipients('exclude', allRootIds, selectedRootIds);
    // 全部 f1..f5 − {f1,f3,f5,f4} = f2
    expect(recipients).toEqual(['f2']);
  });

  it('private selection yields empty delivery (local only)', () => {
    const { allRootIds, selectedRootIds } = expandRecipientsFromSelection('private', allSelected(), friends);
    expect(allRootIds).toEqual([]);
    expect(selectedRootIds).toEqual([]);
  });

  // ------------------------------------------------------------------
  // payload 编解码往返
  // ------------------------------------------------------------------

  it('round-trips post payload encode/decode', () => {
    const post = mkPost({ id: 'post-9', authorRootId: 'root-a', text: '正文', createdAt: 9 });
    const enc = encodePostPayload(post, 'sig-x', 'pk-y');
    const decoded = decodePostPayload(enc);
    expect(decoded.post).toEqual(post);
    expect(decoded.sig).toBe('sig-x');
    expect(decoded.pubKey).toBe('pk-y');
    // 验签前置字段提取：解码后可从 post 重算载荷
    expect(buildPostSignPayload(decoded.post)).toBe(buildPostSignPayload(post));
  });

  it('round-trips interaction payload encode/decode', () => {
    const enc = encodeInteractionPayload('post-9', { type: 'comment', text: '好看', action: 'add', ts: 1 }, ['f3', 'f5'], 'sig-i', 'pk-i');
    const decoded = decodeInteractionPayload(enc);
    expect(decoded.postId).toBe('post-9');
    expect(decoded.interaction.text).toBe('好看');
    expect(decoded.broadcast).toEqual(['f3', 'f5']);
    // author 收件标记
    expect(decodeInteractionPayload(encodeInteractionPayload('p', { type: 'like', action: 'add', ts: 1 }, 'author', 's', 'k')).broadcast).toBe('author');
  });

  it('round-trips delete payload encode/decode', () => {
    const enc = encodeDeletePayload('post-9', 'root-a', 'sig-d', 'pk-d');
    const decoded = decodeDeletePayload(enc);
    expect(decoded.postId).toBe('post-9');
    expect(decoded.authorRootId).toBe('root-a');
    expect(decoded.sig).toBe('sig-d');
  });

  it('throws on malformed payload', () => {
    expect(() => decodePostPayload('not-json')).toThrow();
    expect(() => decodePostPayload(JSON.stringify({ sig: 'x', pubKey: 'y' }))).toThrow();
  });
});
