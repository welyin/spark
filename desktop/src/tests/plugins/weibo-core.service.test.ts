import { describe, expect, it, vi } from 'vitest';
import { WeiboCoreService, WEIBO_COLLECTIONS } from '../../../../plugins/weibo-core/service';

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
    }
  } as any;
}

describe('weibo-core service', () => {
  it('declares collection sync strategies before writing (lww config, append-only content)', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValueOnce(null);

    const service = new WeiboCoreService(sdk);
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

    const service = new WeiboCoreService(sdk);
    const config = await service.ensureOrgConfig('org-1', 'root-admin');

    expect(config.orgId).toBe('org-1');
    expect(config.superAdminRootId).toBe('root-admin');
    expect(sdk.docs.put).toHaveBeenCalledTimes(1);
    expect(sdk.docs.put.mock.calls[0][0]).toBe(WEIBO_COLLECTIONS.orgConfig);
    expect(sdk.docs.put.mock.calls[0][1]).toBe('org-1');
  });

  it('creates comments and replies with parent relation', async () => {
    const sdk = createMockSdk();
    const service = new WeiboCoreService(sdk);

    const comment = await service.createComment('org-1', 'post-1', 'root-member', 'hello');
    const reply = await service.createComment('org-1', 'post-1', 'root-member-2', 'reply', comment.id);

    expect(comment.postId).toBe('post-1');
    expect(comment.parentCommentId).toBeUndefined();
    expect(reply.parentCommentId).toBe(comment.id);
    expect(sdk.docs.put).toHaveBeenCalledTimes(2);
  });

  it('allows admin to create posts but rejects member publishing', async () => {
    const sdk = createMockSdk();
    const service = new WeiboCoreService(sdk);

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

    const service = new WeiboCoreService(sdk);
    await service.loadPosts('org-xyz');
    await service.loadComments('org-xyz');

    expect(sdk.docs.query.mock.calls[0][0]).toBe(WEIBO_COLLECTIONS.posts);
    expect(sdk.docs.query.mock.calls[0][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
    expect(sdk.docs.query.mock.calls[1][0]).toBe(WEIBO_COLLECTIONS.comments);
    expect(sdk.docs.query.mock.calls[1][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
  });
});
