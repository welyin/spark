import { describe, expect, it, vi } from 'vitest';
import { WeiboService, WEIBO_COLLECTIONS } from '../service';
import { buildPostSignPayload } from '../model';

/**
 * mock SDK：覆盖本插件用到的全部域（docs / identity / messages），
 * 与插件能力面一一对应——新加 SDK 调用时先在这里补 mock。
 */
function createMockSdk() {
  return {
    docs: {
      get: vi.fn(),
      put: vi.fn(),
      query: vi.fn(),
      defineCollection: vi.fn().mockResolvedValue({
        collection: 'mock',
        syncStrategy: 'append-only',
        governance: false,
        enableEvidence: true
      })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({
        domain: 'plugin:spark-example',
        domainId: 'spark-example',
        publicKey: 'pk-1',
        signature: 'sig-1',
        payloadHash: 'ph-1'
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    messages: {
      sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }),
      listAppMessages: vi.fn(),
      markRead: vi.fn()
    }
  } as any;
}

describe('spark-example service', () => {
  it('declares collection sync strategies before writing (lww config, append-only content)', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValueOnce(null);

    const service = new WeiboService(sdk);
    await service.ensureOrgConfig('org-1', 'root-admin');
    await service.createPost('org-1', 'root-admin', 'hello', 'admin');

    const declared = sdk.docs.defineCollection.mock.calls.map((call: any[]) => [call[0], call[1]]);
    expect(declared).toEqual([
      [WEIBO_COLLECTIONS.orgConfig, { syncStrategy: 'lww' }],
      [WEIBO_COLLECTIONS.posts, { syncStrategy: 'append-only' }],
      [WEIBO_COLLECTIONS.comments, { syncStrategy: 'append-only' }]
    ]);
    // 声明幂等：第二次写入不再重复声明
    expect(sdk.docs.defineCollection).toHaveBeenCalledTimes(3);
  });

  it('sets creator as super admin on first org config', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValueOnce(null);

    const service = new WeiboService(sdk);
    const config = await service.ensureOrgConfig('org-1', 'root-admin');

    expect(config.orgId).toBe('org-1');
    expect(config.superAdminRootId).toBe('root-admin');
    expect(sdk.docs.put).toHaveBeenCalledTimes(1);
    expect(sdk.docs.put.mock.calls[0][0]).toBe(WEIBO_COLLECTIONS.orgConfig);
    expect(sdk.docs.put.mock.calls[0][1]).toBe('org-1');
  });

  it('creates comments and replies with parent relation', async () => {
    const sdk = createMockSdk();
    const service = new WeiboService(sdk);

    const comment = await service.createComment('org-1', 'post-1', 'root-member', 'hello');
    const reply = await service.createComment('org-1', 'post-1', 'root-member-2', 'reply', comment.id);

    expect(comment.postId).toBe('post-1');
    expect(comment.parentCommentId).toBeUndefined();
    expect(reply.parentCommentId).toBe(comment.id);
    expect(sdk.docs.put).toHaveBeenCalledTimes(2);
  });

  it('allows admin to create posts but rejects member publishing', async () => {
    const sdk = createMockSdk();
    const service = new WeiboService(sdk);

    await expect(service.createPost('org-1', 'root-admin', 'hello', 'admin')).resolves.toMatchObject({
      orgId: 'org-1',
      authorRootId: 'root-admin',
      content: 'hello'
    });

    await expect(service.createPost('org-1', 'root-member', 'should fail', 'member')).rejects.toThrow(/admins/i);
  });

  it('queries by orgId to keep cross-device sync scope stable', async () => {
    const sdk = createMockSdk();
    sdk.docs.query.mockResolvedValue({ items: [], nextCursor: undefined });

    const service = new WeiboService(sdk);
    await service.loadPosts('org-xyz');
    await service.loadComments('org-xyz');

    expect(sdk.docs.query.mock.calls[0][0]).toBe(WEIBO_COLLECTIONS.posts);
    expect(sdk.docs.query.mock.calls[0][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
    expect(sdk.docs.query.mock.calls[1][0]).toBe(WEIBO_COLLECTIONS.comments);
    expect(sdk.docs.query.mock.calls[1][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
  });

  // ------------------------------------------------------------------
  // identity:sign（发帖防抵赖）
  // ------------------------------------------------------------------

  it('signs post content with domain identity and stores signature on the post', async () => {
    const sdk = createMockSdk();
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '签名正文', 'admin');

    // 签名载荷绑定 org+post+内容哈希（防剪贴重放）
    expect(sdk.identity.sign).toHaveBeenCalledWith(buildPostSignPayload('org-1', post.id, '签名正文'));
    expect(post.signature).toEqual({
      payload: buildPostSignPayload('org-1', post.id, '签名正文'),
      signature: 'sig-1',
      publicKey: 'pk-1'
    });
    // 签名随帖落库（append-only 集合，随同步分发给全体成员）
    const stored = sdk.docs.put.mock.calls.find((call: any[]) => call[0] === WEIBO_COLLECTIONS.posts);
    expect(stored[2].signature).toEqual(post.signature);
  });

  it('degrades to unsigned post when identity:sign is rejected (permission denial must not block)', async () => {
    const sdk = createMockSdk();
    sdk.identity.sign.mockRejectedValueOnce(new Error('Access denied: identity:sign rejected by user'));
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '拒绝签名也照发', 'admin');

    expect(post.signature).toBeUndefined();
    const stored = sdk.docs.put.mock.calls.find((call: any[]) => call[0] === WEIBO_COLLECTIONS.posts);
    expect(stored[2].signature).toBeUndefined();
  });

  it('verifies signature via permission-free identity.verify', async () => {
    const sdk = createMockSdk();
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '待验签', 'admin');
    await expect(service.verifyPostSignature(post)).resolves.toBe(true);
    expect(sdk.identity.verify).toHaveBeenCalledWith(post.signature.payload, 'sig-1', 'pk-1');

    sdk.identity.verify.mockResolvedValueOnce({ valid: false });
    await expect(service.verifyPostSignature(post)).resolves.toBe(false);

    // 旧帖/未签名帖：无签名直接 false，不调用 verify
    const unsigned = { ...post, signature: undefined };
    await expect(service.verifyPostSignature(unsigned)).resolves.toBe(false);
  });

  // ------------------------------------------------------------------
  // message:app（应用会话通知 + 卡片数据流）
  // ------------------------------------------------------------------

  it('sends app message with mandatory summary and post-card reference after posting', async () => {
    const sdk = createMockSdk();
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '新帖正文', 'admin');
    await service.notifyNewPost(post);

    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    const [payload, card] = sdk.messages.sendAppMessage.mock.calls[0];
    // 声明式摘要（§20 强制）：未装插件时壳层原生渲染这段文本
    expect(payload.summary).toBe('【新帖】新帖正文');
    // 卡片只携带引用：正文经 docs 查询，不随消息冗余落库
    expect(card).toEqual({ viewId: 'post-card', data: { postId: post.id } });
  });

  it('degrades silently when app message is denied or rate-limited', async () => {
    const sdk = createMockSdk();
    sdk.messages.sendAppMessage.mockRejectedValueOnce(new Error('rate-limited'));
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '限流降级', 'admin');
    await expect(service.notifyNewPost(post)).resolves.toBeUndefined();
  });

  it('skips app message when messages module is absent (non-bridge context)', async () => {
    const sdk = createMockSdk();
    delete sdk.messages;
    const service = new WeiboService(sdk);

    const post = await service.createPost('org-1', 'root-admin', '无桥环境', 'admin');
    await expect(service.notifyNewPost(post)).resolves.toBeUndefined();
  });
});
