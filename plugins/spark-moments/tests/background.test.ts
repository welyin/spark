import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  buildDeleteSignPayload,
  buildInteractionSignPayload,
  buildPostSignPayload,
  type MomentsInteraction,
  type MomentsPost,
  type MomentsProfile
} from '../model';

/**
 * 朋友圈后台脚本（background.ts）的单元测试：
 * 用**假宿主**（内核 PRELUDE 注入的 spark API 的内存实现）驱动真实插件脚本，
 * 验证三通道收件（post/interaction/delete）验签落库 + 互动广播 + 应用会话通知。
 *
 * 脚本经动态 import 执行（顶层副作用即「入口」：声明集合 + 订阅 onReceive + pull）。
 * 每次用例前 resetModules + 换新的假宿主，保证用例间互不影响。
 * `spark` 在脚本中是自由变量，运行时解析到 globalThis.spark（与 QuickJS 沙箱同构）。
 */

type FeedMsg = { topic: string; payload: unknown; from: string; ts: number };

/** 假宿主：内核 plugin/runtime.rs PRELUDE 的 spark API 契约的内存实现 */
function createFakeHost() {
  const postsStore = new Map<string, Record<string, unknown>>();
  const interactionsStore = new Map<string, unknown>();
  const calls = {
    logs: [] as string[],
    savedPosts: [] as Array<{ key: string; value: unknown }>,
    savedInteractions: [] as Array<{ key: string; value: unknown }>,
    delivers: [] as Array<{ topic: string; payload: unknown; recipients: string[]; replyTo?: string }>,
    appMessages: [] as Array<{ summary: string; card?: { viewId: string; data: unknown } }>
  };
  const receiveHandlers = new Map<string, (msg: FeedMsg) => void>();

  const fake = {
    pluginId: 'spark-moments',
    log: (msg: string) => {
      calls.logs.push(msg);
    },
    data: {
      declareCollection: () => ({}),
      save: (name: string, key: string, value: unknown) => {
        if (name === 'spark-moments:posts') {
          postsStore.set(key, value as Record<string, unknown>);
          calls.savedPosts.push({ key, value });
        } else if (name === 'spark-moments:interactions') {
          interactionsStore.set(key, value);
          calls.savedInteractions.push({ key, value });
        }
      },
      get: (name: string, key: string) => {
        if (name === 'spark-moments:posts') return postsStore.get(key) ?? null;
        if (name === 'spark-moments:interactions') return interactionsStore.get(key) ?? null;
        return null;
      }
    },
    feed: {
      onReceive: (topic: string, handler: (msg: FeedMsg) => void) => {
        receiveHandlers.set(topic, handler);
      },
      pull: () => ({ items: [], nextCursor: undefined }),
      deliver: (input: { topic: string; payload: unknown; recipients: string[]; replyTo?: string }) => {
        calls.delivers.push({ topic: input.topic, payload: input.payload, recipients: input.recipients, replyTo: input.replyTo });
        return { requested: input.recipients.length, accepted: input.recipients.length };
      }
    },
    identity: {
      sign: () => ({ domain: 'plugin:spark-moments', domainId: 'd1', publicKey: 'pk-1', signature: 'sig-1', payloadHash: 'ph-1' }),
      verify: () => true
    },
    messages: {
      sendAppMessage: (input: { summary: string; card?: { viewId: string; data: unknown } }) => {
        calls.appMessages.push(input);
      }
    }
  };

  return {
    fake,
    calls,
    store: { posts: postsStore, interactions: interactionsStore },
    /** 向订阅的 onReceive 处理器推送一条 feed 消息 */
    emit(topic: string, payload: unknown, from = 'root-b', ts = 1000) {
      receiveHandlers.get(topic)?.({ topic, payload, from, ts });
    }
  };
}

type FakeHost = ReturnType<typeof createFakeHost>;

/** 加载后台脚本（顶层代码即入口：声明集合 + 订阅 + pull） */
async function loadBackground(host: FakeHost): Promise<void> {
  (globalThis as Record<string, unknown>).spark = host.fake;
  vi.resetModules();
  await import('../background');
}

function makePost(authorRootId: string, overrides: Partial<MomentsPost> = {}): MomentsPost {
  const post: MomentsPost = {
    id: 'post-1',
    authorRootId,
    text: '正文',
    images: [],
    createdAt: 100,
    visibleScope: 'all',
    visibleList: [],
    recipients: ['root-b', 'root-c'],
    ...overrides
  };
  post.signature = { payload: buildPostSignPayload(post), signature: 'sig-1', publicKey: 'pk-1' };
  return post;
}

describe('spark-moments background script', () => {
  let host: FakeHost;

  beforeEach(() => {
    host = createFakeHost();
  });

  afterEach(() => {
    delete (globalThis as Record<string, unknown>).spark;
  });

  // ------------------------------------------------------------------
  // post 通道（验签落库）
  // ------------------------------------------------------------------

  it('收动态：验签通过 → 落 spark-moments:posts', async () => {
    await loadBackground(host);
    const post = makePost('root-a');
    host.emit('spark-moments:post', { post }, 'root-a');
    expect(host.store.posts.get(post.id)).toEqual(post);
  });

  it('收动态：验签失败 → 拒收不落库', async () => {
    await loadBackground(host);
    // 篡改正文使重算载荷与随帖签名载荷不符 → 验签失败
    const tampered = { ...makePost('root-a'), text: '被篡改' };
    host.emit('spark-moments:post', { post: tampered }, 'root-a');
    expect(host.store.posts.size).toBe(0);
  });

  it('收动态：缺 id/authorRootId → 拒收不落库', async () => {
    await loadBackground(host);
    host.emit('spark-moments:post', { post: { text: 'no-id' } }, 'root-a');
    expect(host.store.posts.size).toBe(0);
  });

  // ------------------------------------------------------------------
  // interaction 通道（第一跳验签 + 广播 + 通知）
  // ------------------------------------------------------------------

  it('互动第一跳：验签 → 落库 → 广播名单（除互动者）→ 写应用会话通知', async () => {
    await loadBackground(host);
    // 本机是作者（post 已在 spark-moments:posts），recipients=[b,c]
    const post = makePost('root-me', { recipients: ['root-b', 'root-c'] });
    host.store.posts.set(post.id, post);
    const interaction: MomentsInteraction = { type: 'like', action: 'add', ts: 200 };
    host.emit(
      'spark-moments:interaction',
      {
        postId: post.id,
        type: 'like',
        rootId: 'root-b',
        interaction,
        signature: {
          payload: buildInteractionSignPayload(post.id, 'like', 'root-b', '', 'add'),
          signature: 'sig-1',
          publicKey: 'pk-1'
        }
      },
      'root-b'
    );

    // 落库 key 复合
    expect(host.store.interactions.get('post-1:like:root-b')).toEqual(interaction);
    // 广播给名单除 B 外（recipients 除 root-b）
    const broadcast = host.calls.delivers.find((d) => d.topic === 'spark-moments:interaction');
    expect(broadcast).toBeDefined();
    expect(broadcast!.recipients).toEqual(['root-c']);
    expect(broadcast!.replyTo).toBe('post-1');
    // 应用会话通知
    expect(host.calls.appMessages).toHaveLength(1);
    expect(host.calls.appMessages[0].summary).toContain('赞了你的动态');
    expect(host.calls.appMessages[0].card?.viewId).toBe('notify-card');
  });

  it('互动第一跳：验签失败 → 拒收不落库不广播不通知', async () => {
    await loadBackground(host);
    const post = makePost('root-me', { recipients: ['root-b', 'root-c'] });
    host.store.posts.set(post.id, post);
    // 签名载荷与当前 rootId 不符（篡改 rootId）→ 验签失败
    host.emit(
      'spark-moments:interaction',
      {
        postId: post.id,
        type: 'like',
        rootId: 'root-b',
        interaction: { type: 'like', action: 'add', ts: 200 },
        signature: {
          payload: buildInteractionSignPayload(post.id, 'like', 'root-evil', '', 'add'),
          signature: 'sig-1',
          publicKey: 'pk-1'
        }
      },
      'root-b'
    );
    expect(host.store.interactions.size).toBe(0);
    expect(host.calls.delivers.filter((d) => d.topic === 'spark-moments:interaction')).toHaveLength(0);
    expect(host.calls.appMessages).toHaveLength(0);
  });

  it('互动广播（broadcast=true）：本地增删互动，不广播不通知', async () => {
    await loadBackground(host);
    // 非作者（本机无该动态或非作者），收到作者广播 → 只落库
    const interaction: MomentsInteraction = { type: 'comment', action: 'add', text: '真好看', ts: 300 };
    host.emit(
      'spark-moments:interaction',
      { postId: 'post-1', type: 'comment', rootId: 'root-b', interaction, broadcast: true },
      'root-me'
    );
    expect(host.store.interactions.get('post-1:comment:root-b')).toEqual(interaction);
    expect(host.calls.delivers).toHaveLength(0);
    expect(host.calls.appMessages).toHaveLength(0);
  });

  // ------------------------------------------------------------------
  // delete 通道
  // ------------------------------------------------------------------

  it('删除通知（带签名）：验签通过 → 标记本地 deletedAt', async () => {
    await loadBackground(host);
    const post = makePost('root-a');
    host.store.posts.set(post.id, post);
    const authorRootId = 'root-a';
    const expected = buildDeleteSignPayload(post.id, authorRootId);
    host.emit(
      'spark-moments:delete',
      { postId: post.id, authorRootId, sig: 'sig-1', pubKey: 'pk-1', signature: { payload: expected, signature: 'sig-1', publicKey: 'pk-1' } },
      authorRootId
    );
    const stored = host.store.posts.get(post.id) as MomentsPost;
    expect(stored.deletedAt).toBeDefined();
  });

  it('删除通知（带签名）：验签失败 → 不标记', async () => {
    await loadBackground(host);
    const post = makePost('root-a');
    host.store.posts.set(post.id, post);
    // 签名载荷与 authorRootId 不符（篡改）→ 验签失败
    host.emit(
      'spark-moments:delete',
      { postId: post.id, authorRootId: 'root-a', sig: 'sig-1', pubKey: 'pk-1', signature: { payload: buildDeleteSignPayload(post.id, 'root-evil'), signature: 'sig-1', publicKey: 'pk-1' } },
      'root-a'
    );
    expect((host.store.posts.get(post.id) as MomentsPost).deletedAt).toBeUndefined();
  });

  it('删除通知（旧版无签名）：降级标记 deletedAt', async () => {
    await loadBackground(host);
    const post = makePost('root-a');
    host.store.posts.set(post.id, post);
    host.emit('spark-moments:delete', { postId: post.id, deletedAt: 500 }, 'root-a');
    expect((host.store.posts.get(post.id) as MomentsPost).deletedAt).toBe(500);
  });
});
