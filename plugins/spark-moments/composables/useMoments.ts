/**
 * 朋友圈插件（spark-moments）· 数据收敛组合式函数。
 *
 * 收敛：SDK 初始化、service 实例、feed 订阅（onReceive + pull 补读）、
 * 时间线/互动/我的 profile 状态。视图组件只经本 composable 读写数据，
 * 不直接碰 sdk.data / sdk.feed 细节（对齐代码规范 §四 分层收敛）。
 *
 * feed 收件双路径：
 * - 在线推送 sdk.feed.onReceive（按 topic 分发到 service 对应接收方法）；
 * - 启动 pull 补读（崩溃/未运行期间进入收件箱的数据，social-feed §8）。
 * 两种路径均先 identity.verify 验签后落库（防伪造硬约束，见 service.receive*）。
 */

import { computed, readonly, ref } from 'vue';
import { ensurePluginSDK, type PluginSDK } from '../../../packages/plugin-sdk/src';
import { MomentsService, MOMENTS_TOPICS } from '../service';
import type { MomentsInteraction, MomentsPost, MomentsProfile } from '../model';

export type InteractionEntry = { key: string; postId: string; type: 'like' | 'comment'; rootId: string; interaction: MomentsInteraction };

/** 单例状态（插件主视图生命周期内全局共享） */
let _sdk: PluginSDK | null = null;
let _service: MomentsService | null = null;
let _initPromise: Promise<void> | null = null;

/**
 * 全局共享状态：身份 / profile / 时间线均为模块级单例，而不是实例级。
 *
 * 原因：`init()` 通过 `_initPromise` 只执行一次，且只在首次调用 `init()` 的那个
 * `useMoments()` 实例上填充局部 ref。若这些状态是实例级的，其它视图（ComposerView /
 * VisibilityPickerView / PostDetailView …）各自持有独立的 `myRootId = null`，从未被
 * 填充，一旦在那些实例上调用 `publish` 就会误报「身份未解锁」。提升为模块级单例后，
 * 所有 `useMoments()` 实例共享同一份身份与时间线状态。
 */
const posts = ref<MomentsPost[]>([]);
const interactionsByPost = ref<Record<string, InteractionEntry[]>>({});
const selfProfile = ref<MomentsProfile | null>(null);
const myRootId = ref<string | null>(null);
const ready = ref(false);
const offline = ref(false);

export function useMoments() {

  /** 各动态的点赞 rootId 列表（按点赞时间正序） */
  const likersByPost = computed<Record<string, string[]>>(() => {
    const result: Record<string, string[]> = {};
    for (const post of posts.value) {
      result[post.id] = collectLikers(interactionsByPost.value[post.id] ?? [], post.id);
    }
    return result;
  });

  /** 各动态的评论列表（按时间正序） */
  const commentsByPost = computed<Record<string, InteractionEntry[]>>(() => {
    const result: Record<string, InteractionEntry[]> = {};
    for (const post of posts.value) {
      result[post.id] = collectComments(interactionsByPost.value[post.id] ?? [], post.id);
    }
    return result;
  });

  async function ensureSdk(): Promise<{ sdk: PluginSDK; service: MomentsService }> {
    if (!_sdk || !_service) {
      _sdk = await ensurePluginSDK();
      _service = new MomentsService(_sdk);
    }
    return { sdk: _sdk, service: _service };
  }

  /** 初始化：SDK + service + 我的身份 + 时间线 + feed 订阅 + pull 补读 */
  async function init(): Promise<void> {
    if (!_initPromise) {
      _initPromise = (async () => {
        const { sdk, service } = await ensureSdk();
        const identity = await sdk.runtime.currentRoot();
        myRootId.value = identity.unlocked ? identity.rootId : null;
        selfProfile.value = await service.getSelfProfile();
        // 本地 profile 为空时，用「我的身份」昵称/头像初始化（右上角头像展示；
        // 身份头像/昵称变更后这里用旧值，需在身份设置页更新时同步写入本地）。
        if (!selfProfile.value && identity.nickname) {
          const seeded: MomentsProfile = {
            nickname: identity.nickname,
            avatar: identity.avatar ?? undefined,
            updatedAt: Date.now()
          };
          selfProfile.value = seeded;
          await service.saveSelfProfile(seeded);
        }

        await subscribeFeed(service);
        await refreshTimeline();

        // pull 补读：启动/恢复路径，拉取收件箱未消费 feed
        await pullInbox(service);
        ready.value = true;
      })();
    }
    return _initPromise;
  }

  /**
   * 确保「我的身份」已加载，返回当前 myRootId。
   *
   * 先等 init() 完成（避免在身份尚在异步加载时就校验，误报未解锁），
   * 再判断身份是否真正解锁。返回 `string`（非空），未解锁则抛错。
   */
  async function ensureIdentity(): Promise<string> {
    await init();
    if (!myRootId.value) throw new Error('身份未解锁，请先在系统设置中解锁身份');
    return myRootId.value;
  }

  /** 订阅在线 feed（三 topic 分发；接收侧免权限） */
  async function subscribeFeed(service: MomentsService): Promise<void> {
    const { sdk } = await ensureSdk();
    if (!sdk.feed) return;

    await sdk.feed.onReceive(MOMENTS_TOPICS.post, (msg) => {
      void service.receivePost(msg.payload).then((accepted) => {
        if (accepted) void refreshTimeline();
      });
    });

    await sdk.feed.onReceive(MOMENTS_TOPICS.interaction, (msg) => {
      // 广播型（作者发出的，非作者共同好友可见）与作者收件型（第一跳）共用一个 topic，
      // 用 payload.broadcast 区分：true = 广播（非作者本地增删），否则 = 作者收件（广播名单 + 通知）
      const payload = msg.payload as { broadcast?: boolean };
      if (payload.broadcast === true) {
        void service.receiveInteractionBroadcast(msg.payload).then(() => {
          void refreshTimeline();
        });
      } else {
        void service.receiveInteraction(msg.payload).then((accepted) => {
          if (accepted) {
            void refreshTimeline();
            void notifyAuthor(msg.payload);
          }
        });
      }
    });

    await sdk.feed.onReceive(MOMENTS_TOPICS.delete, (msg) => {
      void service.receiveDelete(msg.payload).then(() => {
        void refreshTimeline();
      });
    });
  }

  /** 作者收到互动后生成应用通知（message:app，限流降级） */
  async function notifyAuthor(payload: unknown): Promise<void> {
    const msg = payload as {
      postId?: string;
      type?: 'like' | 'comment';
      rootId?: string;
      interaction?: MomentsInteraction;
    };
    if (!msg?.postId || !msg.type || !msg.interaction) return;
    const { sdk, service } = await ensureSdk();
    const post = posts.value.find((p) => p.id === msg.postId) ?? (await service.getPost(msg.postId));
    if (!post) return;
    const friends = sdk.contacts ? await sdk.contacts.listFriends() : [];
    const fromName = service.displayNameOf(friends, msg.rootId ?? '', post.authorSnapshot);
    await service.notifyInteraction({
      kind: msg.type,
      fromRootIds: [msg.rootId ?? ''],
      fromName,
      fromAvatar: null,
      count: 1,
      postId: post.id,
      postExcerpt: post.text ? buildExcerpt(post.text) : '[图片]',
      postThumbHash: post.images[0]?.thumbHash ?? null,
      commentExcerpt: msg.type === 'comment' ? (msg.interaction.text ?? '') : undefined,
      ts: msg.interaction.ts
    });
  }

  /** pull 补读收件箱（按 topic 分发到同一接收路径） */
  async function pullInbox(service: MomentsService): Promise<void> {
    const { sdk } = await ensureSdk();
    if (!sdk.feed) return;
    const topics = [MOMENTS_TOPICS.post, MOMENTS_TOPICS.interaction, MOMENTS_TOPICS.delete];
    for (const topic of topics) {
      let cursor: string | undefined;
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const res = await sdk.feed.pull({ topic, cursor, limit: 50 });
        for (const item of res.items) {
          if (topic === MOMENTS_TOPICS.post) await service.receivePost(item.payload);
          else if (topic === MOMENTS_TOPICS.interaction) {
            const payload = item.payload as { broadcast?: boolean };
            if (payload.broadcast === true) await service.receiveInteractionBroadcast(item.payload);
            else await service.receiveInteraction(item.payload);
          } else if (topic === MOMENTS_TOPICS.delete) await service.receiveDelete(item.payload);
        }
        if (!res.nextCursor) break;
        cursor = res.nextCursor;
      }
    }
    await refreshTimeline();
  }

  /** 刷新时间线 + 互动表 */
  async function refreshTimeline(): Promise<void> {
    const { service } = await ensureSdk();
    posts.value = await service.loadTimeline();
    const interactions: Record<string, InteractionEntry[]> = {};
    for (const post of posts.value) {
      const entries = await service.loadInteractions(post.id);
      interactions[post.id] = entries.map((item) => parseInteractionKey(item.key, item.interaction));
    }
    interactionsByPost.value = interactions;
  }

  /** 发动态（由 ComposerView 调用） */
  async function publish(input: {
    text: string;
    images: Parameters<MomentsService['publishPost']>[0]['images'];
    scope: MomentsPost['visibleScope'];
    selection: { contactRootIds: string[]; groupIds: string[]; tagIds: string[] };
  }): Promise<MomentsPost> {
    const { service } = await ensureSdk();
    const rootId = await ensureIdentity();
    const profile = selfProfile.value ?? { nickname: '我', updatedAt: 0 };
    const post = await service.publishPost({
      ...input,
      myRootId: rootId,
      myProfile: profile
    });
    await refreshTimeline();
    return post;
  }

  /** 互动（点赞/评论/取消） */
  async function interact(post: MomentsPost, type: 'like' | 'comment', action: 'add' | 'remove', text?: string): Promise<void> {
    const { service } = await ensureSdk();
    const rootId = await ensureIdentity();
    await service.interact({ post, type, action, text, myRootId: rootId });
    await refreshTimeline();
  }

  /** 删除动态 */
  async function deletePost(post: MomentsPost): Promise<void> {
    const { service } = await ensureSdk();
    await service.deletePost(post);
    await refreshTimeline();
  }

  /** 删除评论 */
  async function deleteComment(post: MomentsPost, commentRootId: string): Promise<void> {
    const { service } = await ensureSdk();
    const rootId = await ensureIdentity();
    await service.deleteComment(post, commentRootId, rootId);
    await refreshTimeline();
  }

  /** 通讯录（供名单编辑器 / 展示名解析） */
  async function loadContacts() {
    const { sdk } = await ensureSdk();
    if (!sdk.contacts) return { friends: [], groups: [], tags: [] };
    const [friends, groups, tags] = await Promise.all([
      sdk.contacts.listFriends(),
      sdk.contacts.listGroups(),
      sdk.contacts.listTags()
    ]);
    return { friends, groups, tags };
  }

  /** 仅看动态：我的动态 */
  async function loadMyPosts(): Promise<MomentsPost[]> {
    const { service } = await ensureSdk();
    const all = await service.loadTimeline();
    return all.filter((post) => post.authorRootId === myRootId.value);
  }

  /** ta 的动态：某联系人发给我的且可见的动态 */
  async function loadPostsByAuthor(authorRootId: string): Promise<MomentsPost[]> {
    const { service } = await ensureSdk();
    const all = await service.loadTimeline();
    return all.filter((post) => post.authorRootId === authorRootId);
  }

  /** 单条动态（详情页定位用；未命中或已删除由调用方判 notFound） */
  async function getPost(postId: string): Promise<MomentsPost | null> {
    const { service } = await ensureSdk();
    return service.getPost(postId);
  }

  return {
    posts,
    interactionsByPost,
    likersByPost,
    commentsByPost,
    selfProfile,
    myRootId,
    ready,
    offline,
    init,
    refreshTimeline,
    publish,
    interact,
    deletePost,
    deleteComment,
    loadContacts,
    loadMyPosts,
    loadPostsByAuthor,
    getPost,
    ensureSdk
  };
}

/** 解析互动复合键 `{postId}:{type}:{rootId}` 为结构 */
function parseInteractionKey(key: string, interaction: MomentsInteraction): InteractionEntry {
  const [postId, type, ...rest] = key.split(':');
  return {
    key,
    postId,
    type: type === 'comment' ? 'comment' : 'like',
    rootId: rest.join(':'),
    interaction
  };
}

/** 收集某动态的点赞 rootId（按点赞时间正序） */
function collectLikers(entries: InteractionEntry[], _postId: string): string[] {
  return entries
    .filter((e) => e.type === 'like')
    .filter((e) => e.interaction.action === 'add')
    .sort((a, b) => a.interaction.ts - b.interaction.ts)
    .map((e) => e.rootId);
}

/** 收集某动态的评论（按时间正序，action=add） */
function collectComments(entries: InteractionEntry[], _postId: string): InteractionEntry[] {
  return entries
    .filter((e) => e.type === 'comment' && e.interaction.action === 'add')
    .sort((a, b) => a.interaction.ts - b.interaction.ts);
}

function buildExcerpt(text: string, max = 60): string {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}…`;
}
