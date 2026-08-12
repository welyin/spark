import { describe, expect, it, vi } from 'vitest';
import { MomentsService, MOMENTS_TOPICS } from '../service';
import { buildPostSignPayload, type MomentsPost, type MomentsProfile } from '../model';

/**
 * mock SDK：覆盖本插件用到的全部域（data / feed / identity / contacts / messages），
 * 与插件能力面一一对应——新加 SDK 调用时先在这里补 mock。
 */
function createMockSdk(overrides: Record<string, unknown> = {}) {
  const save = vi.fn().mockResolvedValue({ success: true });
  const deliver = vi.fn().mockResolvedValue({ requested: 1, accepted: 1 });
  const get = vi.fn().mockResolvedValue(null);
  return {
    domain: 'plugin:spark-moments',
    data: {
      declareCollection: vi.fn().mockResolvedValue({}),
      save,
      get,
      query: vi.fn().mockResolvedValue({ items: [], nextCursor: undefined }),
      saveBlob: vi.fn().mockResolvedValue({ hash: 'h', size: 1 }),
      readBlob: vi.fn(),
      delete: vi.fn()
    },
    feed: { deliver, onReceive: vi.fn(), pull: vi.fn() },
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig-1', publicKey: 'pk-1', payloadHash: 'ph-1' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    contacts: {
      listFriends: vi.fn().mockResolvedValue([
        { rootId: 'root-b', nickname: '张三', groupId: 'g1', tagIds: ['t1'] },
        { rootId: 'root-c', nickname: '李四', groupId: 'g2', tagIds: [] },
        { rootId: 'root-d', nickname: '王五', groupId: '', tagIds: [] }
      ]),
      listGroups: vi.fn().mockResolvedValue([{ id: 'g1', name: '家人' }, { id: 'g2', name: '同事' }]),
      listTags: vi.fn().mockResolvedValue([{ id: 't1', name: '好友' }])
    },
    messages: { sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }) },
    ...overrides
  } as any;
}

const MY_PROFILE: MomentsProfile = { nickname: '我', updatedAt: 1 };

describe('spark-moments service', () => {
  it('declares collections before first write', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    await service.publishPost({
      text: 'hi',
      images: [],
      scope: 'private',
      selection: { contactRootIds: [], groupIds: [], tagIds: [] },
      myRootId: 'root-me',
      myProfile: MY_PROFILE
    });
    const declared = sdk.data.declareCollection.mock.calls.map((c: any[]) => c[0].name);
    expect(declared).toEqual(['spark-moments:posts', 'spark-moments:interactions', 'spark-moments:profile']);
  });

  // ------------------------------------------------------------------
  // 可见性展开（产品 §5.3）
  // ------------------------------------------------------------------

  it('publishes public post delivering to all friends', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    await service.publishPost({
      text: '公开',
      images: [],
      scope: 'all',
      selection: { contactRootIds: [], groupIds: [], tagIds: [] },
      myRootId: 'root-me',
      myProfile: MY_PROFILE
    });
    const post = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(post.recipients).toEqual(['root-b', 'root-c', 'root-d']);
    expect(sdk.feed.deliver).toHaveBeenCalledWith({
      topic: MOMENTS_TOPICS.post,
      payload: { post },
      recipients: ['root-b', 'root-c', 'root-d']
    });
  });

  it('publishes private post without any delivery (local only)', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    await service.publishPost({
      text: '私密',
      images: [],
      scope: 'private',
      selection: { contactRootIds: [], groupIds: [], tagIds: [] },
      myRootId: 'root-me',
      myProfile: MY_PROFILE
    });
    const post = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(post.recipients).toEqual([]);
    expect(sdk.feed.deliver).not.toHaveBeenCalled();
  });

  it('publishes partial post delivering only to selected contacts', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    await service.publishPost({
      text: '部分可见',
      images: [],
      scope: 'partial',
      selection: { contactRootIds: ['root-b'], groupIds: [], tagIds: [] },
      myRootId: 'root-me',
      myProfile: MY_PROFILE
    });
    const post = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(post.recipients).toEqual(['root-b']);
  });

  it('publishes exclude post delivering to all except excluded', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    await service.publishPost({
      text: '不给谁看',
      images: [],
      scope: 'exclude',
      selection: { contactRootIds: ['root-c'], groupIds: [], tagIds: [] },
      myRootId: 'root-me',
      myProfile: MY_PROFILE
    });
    const post = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(post.recipients).toEqual(['root-b', 'root-d']);
  });

  it('expands group and tag selections to member root ids', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    // 勾选分组 g1（含 root-b）+ 标签 t1（含 root-b）+ 联系人 root-d → 展开 {b, d}
    const { selectedRootIds } = await service.resolveContactsSelection({
      contactRootIds: ['root-d'],
      groupIds: ['g1'],
      tagIds: ['t1']
    });
    expect(selectedRootIds).toEqual(['root-b', 'root-d']);
  });

  // ------------------------------------------------------------------
  // 收动态：验签硬约束（防伪造）
  // ------------------------------------------------------------------

  it('receives a signed post and stores it', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post = makeSignedPost('root-a');
    await expect(service.receivePost(post)).resolves.toBe(true);
    // verify 收到的是从帖子当前字段重算的期望载荷，而非随帖回放
    const expected = buildPostSignPayload(post);
    expect(sdk.identity.verify).toHaveBeenCalledWith(expected, 'sig-1', 'pk-1');
    expect(sdk.data.save).toHaveBeenCalledWith('spark-moments:posts', post.id, post);
  });

  it('rejects unsigned post', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post: MomentsPost = makeSignedPost('root-a');
    delete post.signature;
    await expect(service.receivePost(post)).resolves.toBe(false);
    expect(sdk.data.save).not.toHaveBeenCalled();
  });

  it('rejects post whose stored payload no longer matches content (tampered)', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post = makeSignedPost('root-a');
    const tampered = { ...post, text: '被篡改' };
    await expect(service.receivePost(tampered)).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();
    expect(sdk.data.save).not.toHaveBeenCalled();
  });

  it('rejects post when signature verification fails', async () => {
    const sdk = createMockSdk();
    sdk.identity.verify.mockResolvedValueOnce({ valid: false });
    const service = new MomentsService(sdk);
    await expect(service.receivePost(makeSignedPost('root-a'))).resolves.toBe(false);
    expect(sdk.data.save).not.toHaveBeenCalled();
  });

  // ------------------------------------------------------------------
  // 互动：作者自己的互动直接广播名单（产品 §6.2）
  // ------------------------------------------------------------------

  it('author interacting with own post broadcasts to recipients except self', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post: MomentsPost = makeSignedPost('root-me', { recipients: ['root-b', 'root-c'] });
    await service.interact({ post, type: 'like', action: 'add', myRootId: 'root-me' });
    // 落库 key 复合
    expect(sdk.data.save.mock.calls[0][1]).toBe('post-1:like:root-me');
    // 广播给名单（除自己）
    expect(sdk.feed.deliver).toHaveBeenCalledWith(
      expect.objectContaining({
        topic: MOMENTS_TOPICS.interaction,
        recipients: ['root-b', 'root-c'],
        replyTo: 'post-1'
      })
    );
  });

  it('non-author interacting delivers to author only (first hop)', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post: MomentsPost = makeSignedPost('root-a', { recipients: ['root-b'] });
    await service.interact({ post, type: 'comment', action: 'add', text: '真好看', myRootId: 'root-b' });
    // 投递作者（第一跳）
    const deliverCall = sdk.feed.deliver.mock.calls[0][0];
    expect(deliverCall.topic).toBe(MOMENTS_TOPICS.interaction);
    expect(deliverCall.recipients).toEqual(['root-a']);
    expect(deliverCall.replyTo).toBe('post-1');
    // 投递载荷带签名（作者验签后才会广播）
    expect(deliverCall.payload.signature).toBeDefined();
  });

  // ------------------------------------------------------------------
  // 互动广播接收与再广播（产品 §6.2 第二跳）
  // ------------------------------------------------------------------

  it('author receiving interaction broadcasts to recipients except the interaction author', async () => {
    const sdk = createMockSdk();
    // 作者 root-me 有 post-1，recipients=[b,c,d]；B 点赞
    sdk.data.get.mockResolvedValue(makeSignedPost('root-me', { id: 'post-1', recipients: ['root-b', 'root-c', 'root-d'] }));
    const service = new MomentsService(sdk);
    const accepted = await service.receiveInteraction({
      postId: 'post-1',
      type: 'like',
      rootId: 'root-b',
      interaction: { type: 'like', action: 'add', ts: 1 },
      signature: { payload: buildInteractionPayload('post-1', 'like', 'root-b'), signature: 'sig-1', publicKey: 'pk-1' }
    });
    expect(accepted).toBe(true);
    // 广播给名单（除 B 外）
    const broadcastCall = sdk.feed.deliver.mock.calls[0][0];
    expect(broadcastCall.topic).toBe(MOMENTS_TOPICS.interaction);
    expect(broadcastCall.recipients).toEqual(['root-c', 'root-d']);
    expect(broadcastCall.payload.broadcast).toBe(true);
  });

  it('rejects interaction with invalid signature (no broadcast)', async () => {
    const sdk = createMockSdk();
    sdk.identity.verify.mockResolvedValueOnce({ valid: false });
    const service = new MomentsService(sdk);
    const accepted = await service.receiveInteraction({
      postId: 'post-1',
      type: 'like',
      rootId: 'root-b',
      interaction: { type: 'like', action: 'add', ts: 1 },
      signature: { payload: buildInteractionPayload('post-1', 'like', 'root-b'), signature: 'sig-bad', publicKey: 'pk-bad' }
    });
    expect(accepted).toBe(false);
    expect(sdk.feed.deliver).not.toHaveBeenCalled();
  });

  // ------------------------------------------------------------------
  // 删除动态（产品 §6.5）
  // ------------------------------------------------------------------

  it('author deletes post marking deletedAt and notifying original recipients', async () => {
    const sdk = createMockSdk();
    const service = new MomentsService(sdk);
    const post: MomentsPost = makeSignedPost('root-me', { recipients: ['root-b', 'root-c'] });
    await service.deletePost(post);
    // 本地标记 deletedAt
    const stored = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(stored.deletedAt).toBeDefined();
    // 投递删除通知给原名单
    expect(sdk.feed.deliver).toHaveBeenCalledWith(
      expect.objectContaining({ topic: MOMENTS_TOPICS.delete, recipients: ['root-b', 'root-c'] })
    );
  });

  it('receiver marks post deleted on delete notification', async () => {
    const sdk = createMockSdk();
    sdk.data.get.mockResolvedValueOnce(makeSignedPost('root-a'));
    const service = new MomentsService(sdk);
    await service.receiveDelete({ postId: 'post-1', deletedAt: 500 });
    const stored = sdk.data.save.mock.calls[0][1] as string;
    expect(stored).toBe('post-1');
    const saved = sdk.data.save.mock.calls[0][2] as MomentsPost;
    expect(saved.deletedAt).toBe(500);
  });

  // ------------------------------------------------------------------
  // 时间线读取
  // ------------------------------------------------------------------

  it('loads timeline sorted newest-first filtering deleted', async () => {
    const sdk = createMockSdk();
    sdk.data.query.mockResolvedValueOnce({
      items: [
        { key: 'p1', value: makeSignedPost('root-a', { id: 'p1', createdAt: 100 }) },
        { key: 'p3', value: makeSignedPost('root-b', { id: 'p3', createdAt: 300 }) },
        { key: 'p2', value: makeSignedPost('root-c', { id: 'p2', createdAt: 200, deletedAt: 500 }) }
      ],
      nextCursor: undefined
    });
    const service = new MomentsService(sdk);
    const posts = await service.loadTimeline();
    expect(posts.map((p) => p.id)).toEqual(['p3', 'p1']);
  });
});

// ---------------------------------------------------------------------------
// 构造已签名动态（测试辅助）
// ---------------------------------------------------------------------------

function makeSignedPost(authorRootId: string, overrides: Partial<MomentsPost> = {}): MomentsPost {
  const post: MomentsPost = {
    id: 'post-1',
    authorRootId,
    text: '正文',
    images: [],
    createdAt: 100,
    visibleScope: 'all',
    visibleList: [],
    recipients: [],
    ...overrides
  };
  post.signature = {
    payload: buildPostSignPayload(post),
    signature: 'sig-1',
    publicKey: 'pk-1'
  };
  return post;
}

function buildInteractionPayload(postId: string, type: 'like' | 'comment', rootId: string): string {
  return `moments:interaction:${postId}:${type}:${rootId}:${hash('')}:add`;
}

function hash(content: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    h ^= content.charCodeAt(i);
    h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
  }
  return h.toString(16).padStart(8, '0');
}
