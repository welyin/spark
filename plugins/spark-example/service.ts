/**
 * 示例插件（spark-example）· 业务服务层。
 *
 * 教学要点：
 * - 所有 SDK 调用集中在服务层，视图组件不直接碰 sdk.docs——便于单测
 *   （tests/ 用 mock SDK 驱动本层）与权限审计（本文件即插件能力面清单）；
 * - 本文件演示四类 SDK 能力的协作：
 *   1) docs（storage:read/write）：集合声明 + 文档读写，同步策略写入前必须声明；
 *   2) runtime（org:read/org:sync 在视图层调用）：组织信息读取与手动同步；
 *   3) identity（identity:sign）：发帖防抵赖签名，验签（verify）免权限；
 *   4) messages（message:app）：新帖应用通知 + 帖子卡片——发帖者路径只是
 *      发帖者本机的即时反馈；成员侧按服务号模型（p2p-messages §20.4.3）
 *      在同步后从本机数据「本地生成」通知（见 notifyTimelinePosts）。
 * - 权限降级原则：签名与应用消息是「增强能力」，授权被拒或限流时
 *   不阻断主流程（帖子照发），仅少一个徽标/少一条通知——插件应对
 *   每一个高级权限调用做好 try/catch 降级。
 */
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildPostSignPayload,
  buildPostSummary,
  normalizeWeiboText,
  type WeiboComment,
  type WeiboPost,
  type WeiboPostSignature
} from './model';

export const WEIBO_COLLECTIONS = {
  orgConfig: 'weibo_org_config',
  posts: 'weibo_posts',
  comments: 'weibo_comments'
} as const;

/**
 * 集合同步策略声明（设计文档 V2 §4.3.4，写入前必须声明）：
 * - orgConfig：组织级配置状态，可被后续管理员调整覆盖，显式声明 lww
 * - posts / comments：内容记录，仅追加、不覆盖、不删除，使用默认 append-only（自动链式存证）
 *
 * 选择理由（教学）：内容型数据要的是「可审计、可溯源」，append-only 配合
 * 链式存证让任何篡改都可被发现；配置型数据要的是「当前生效值」，
 * 覆盖语义（lww）才符合直觉。治理类数据（投票/账目）应另标 governance。
 */
const WEIBO_COLLECTION_SCHEMAS = {
  [WEIBO_COLLECTIONS.orgConfig]: { syncStrategy: 'lww' },
  [WEIBO_COLLECTIONS.posts]: { syncStrategy: 'append-only' },
  [WEIBO_COLLECTIONS.comments]: { syncStrategy: 'append-only' }
} as const;

export type WeiboOrgConfig = {
  orgId: string;
  superAdminRootId: string;
  createdBy: string;
  createdAt: number;
};

type WeiboAuthorRole = 'admin' | 'member' | null | undefined;

function newId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

/**
 * 「已通知帖子」去重台账：按空间（orgId）记录在 localStorage。
 * 应用消息是「本地生成、本地消费」（§20.4.3）——消息本身不同步，去重状态
 * 因此也只须是本机状态；localStorage 足够，无需为此占用 docs 集合。
 * localStorage 不可用（存储被禁的沙箱、隐私模式）时降级为进程内记忆：
 * 去重窗口缩小为当前会话，刷新后可能补发一次，属可接受降级。
 */
const NOTIFIED_KEY_PREFIX = 'spark-example:notified-posts:';
const memoryNotifiedFallback = new Map<string, Set<string>>();

function notifiedStorageKey(orgId: string): string {
  return `${NOTIFIED_KEY_PREFIX}${orgId}`;
}

function loadNotifiedPostIds(orgId: string): Set<string> {
  const key = notifiedStorageKey(orgId);
  try {
    const raw = globalThis.localStorage?.getItem(key);
    if (raw) {
      return new Set(JSON.parse(raw) as string[]);
    }
  } catch {
    /* 存储不可用或数据损坏：走进程内兜底 */
  }
  return new Set(memoryNotifiedFallback.get(key) ?? []);
}

function saveNotifiedPostIds(orgId: string, ids: Set<string>): void {
  const key = notifiedStorageKey(orgId);
  memoryNotifiedFallback.set(key, new Set(ids));
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify([...ids]));
  } catch {
    /* 存储不可用时进程内兜底已记录，忽略 */
  }
}

export class WeiboService {
  private collectionsReady: Promise<void> | null = null;

  constructor(private readonly sdk: PluginSDK) {}

  /** 声明本插件全部集合的同步策略（幂等，重复声明与首次一致即可） */
  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(WEIBO_COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  async ensureOrgConfig(orgId: string, rootId: string): Promise<WeiboOrgConfig> {
    await this.ensureCollectionsDeclared();
    const existing = await this.sdk.docs.get<WeiboOrgConfig>(WEIBO_COLLECTIONS.orgConfig, orgId);
    if (existing) {
      return existing;
    }

    const created: WeiboOrgConfig = {
      orgId,
      superAdminRootId: rootId,
      createdBy: rootId,
      createdAt: Date.now()
    };

    await this.sdk.docs.put(WEIBO_COLLECTIONS.orgConfig, orgId, created as unknown as Record<string, unknown>);
    return created;
  }

  /**
   * 发帖防抵赖签名（identity:sign 演示）。
   *
   * SDK 用法：sdk.identity.sign(payload) 以「插件域身份」签名（域私钥永不
   * 离开内核，插件拿不到私钥本身）；该权限是「使用时询问」高危权限，
   * 用户拒绝时桥会抛错——此处降级为不签名发帖（帖子无「已签名」徽标），
   * 不阻断主流程。
   */
  private async signPost(post: WeiboPost): Promise<WeiboPostSignature | null> {
    // 载荷编入作者身份（{orgId}:{postId}:{authorRootId}:{内容哈希}），
    // 验签侧按同一函数从帖子当前字段重算比对（见 verifyPostSignature）
    const payload = buildPostSignPayload(post.orgId, post.id, post.authorRootId, post.content);
    try {
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-example] 签名被拒或不可用，帖子将不带签名徽标：', error);
      return null;
    }
  }

  /**
   * 发帖后向组织应用会话发通知（message:app 演示，服务号模型 §20）。
   *
   * 语义边界（教学）：这是「发帖者路径」——应用消息**不走网络**
   * （§20.4.3：本地生成、本地消费，同步的是数据不是消息），因此这条
   * 通知只到**发帖者本机**的应用会话，作用是让发帖者立即看到自己的
   * 操作回音；组织其他成员的设备要靠各自插件实例在同步后本地生成
   * 通知（见 notifyTimelinePosts），两条路径靠 localStorage 台账去重。
   *
   * SDK 用法：sdk.messages.sendAppMessage(payload, card)——
   * - payload.summary 是强制的声明式摘要：未装插件的成员设备上壳层原生
   *   渲染这段纯文本，必须自成一体（buildPostSummary 已保证）；
   * - card={viewId:'post-card', data:{postId, orgId}} 挂了插件的设备用
   *   message-card 视图富渲染；data 只放引用（postId）与定位所需的
   *   orgId（卡片回调跨组织定位用，见 ExampleView.handleCardAction），
   *   正文经 docs 查询——卡片数据随应用消息本地落库，放正文会冗余两份
   *   且无法同步更新。
   *
   * 降级：权限被拒/内核限流（10 条/60s）时不阻断发帖，返回 false 由
   * 调用方如实提示（成功文案不得声称「应用会话已收到通知」）。
   *
   * @returns 通知是否成功写入本机应用会话
   */
  async notifyNewPost(post: WeiboPost): Promise<boolean> {
    if (!this.sdk.messages) {
      // tab 同进程模式无 messages 模块（SDK 契约上为可选字段）
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: buildPostSummary(post.content), postId: post.id, orgId: post.orgId },
        { viewId: 'post-card', data: { postId: post.id, orgId: post.orgId } }
      );
      // 记入已通知台账：成员侧本地生成路径（notifyTimelinePosts）不会补发重复通知
      const notified = loadNotifiedPostIds(post.orgId);
      notified.add(post.id);
      saveNotifiedPostIds(post.orgId, notified);
      return true;
    } catch (error) {
      console.warn('[spark-example] 应用消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /**
   * 成员侧「本地生成」通知（服务号模型 §20.4.3 演示）。
   *
   * 为什么需要这条路径：notifyNewPost 只写**发帖者本机**的应用会话——
   * 应用消息不参与 dm 同步、不进存证链，天然无法投递给其他成员。组织
   * 场景的「全员可达」由不变量 §20.4.3 保证：帖子**数据**经 org 同步
   * 到达每台成员设备，各设备上的插件实例从本机数据**各自算出**通知
   * 写入本机会话。本方法就是「算出通知」这一步：对同步后出现、且
   * 本机尚未通知过的帖子逐条生成本机应用消息。
   *
   * 调用时机（视图层）：时间线加载完成后（loadTimeline / 手动同步后）。
   * 去重靠 localStorage 台账（loadNotifiedPostIds），重复加载/重复同步
   * 不会重复通知；发帖者本机的发帖即时通知（notifyNewPost）已先记账，
   * 此处自然跳过，两条路径不打架。
   *
   * 限流配合：内核应用消息限流 10 条/60s（§20.5）。一次同步涌入大量
   * 新帖时逐条发送会触发限流——遇失败即中止本轮（已成功的不重发，
   * 未标记的留待下次加载补齐），避免无效重试打满配额。
   *
   * @returns 本轮实际生成的通知条数
   */
  async notifyTimelinePosts(orgId: string, posts: WeiboPost[]): Promise<number> {
    if (!this.sdk.messages || posts.length === 0) {
      return 0;
    }
    const notified = loadNotifiedPostIds(orgId);
    let sent = 0;
    for (const post of posts) {
      if (notified.has(post.id)) {
        continue;
      }
      const ok = await this.notifyNewPost(post);
      if (!ok) {
        // 限流/权限降级：本轮放弃，未记账的帖子下次加载时再补
        break;
      }
      // notifyNewPost 内部已记账，这里同步内存中的集合用于本轮后续判断
      notified.add(post.id);
      sent += 1;
    }
    return sent;
  }

  async createPost(orgId: string, rootId: string, content: string, authorRole: WeiboAuthorRole): Promise<WeiboPost> {
    if (authorRole !== 'admin') {
      throw new Error('Only organization admins can publish posts');
    }

    await this.ensureCollectionsDeclared();
    const post: WeiboPost = {
      id: newId('post'),
      orgId,
      content: normalizeWeiboText(content),
      authorRootId: rootId,
      createdAt: Date.now()
    };

    // 先签名后落库：签名是帖子内容的一部分（随帖存储、随同步分发）
    const signature = await this.signPost(post);
    if (signature) {
      post.signature = signature;
    }

    await this.sdk.docs.put(WEIBO_COLLECTIONS.posts, post.id, post as unknown as Record<string, unknown>);
    return post;
  }

  /**
   * 验签（identity.verify 演示）：纯函数校验，任何成员可对其他作者的
   * 签名验签——verify 是免权限调用（消息卡片视图也因此能做验签徽标）。
   *
   * 正确姿势是「重算后比对」而非「回放随帖 payload」：签名验证的断言是
   * 「这份签名确实签在了**这条帖子当前的内容**上」。若直接拿随帖存储的
   * signature.payload 去 verify，篡改者把 payload 与 signature 成对替换
   * （或把别人的签名整个搬来）也能通过——验签变成自证循环。因此先从帖子
   * 当前字段重算期望载荷，与随帖 payload 不等即判 false（内容/作者/归属
   * 任一被改都会失配），相等才交给密码学验签。
   *
   * 诚实标注剩余缺口：本演示只证明「签名出自持有 signature.publicKey
   * 对应私钥的域身份」，并未校验该 publicKey 与 authorRootId 的绑定——
   * 这需要查询域身份目录（域内各身份公钥的权威来源），超出演示范围。
   * 生产实现应在 verify 之外补一步「publicKey ∈ 作者域身份集」的校验。
   */
  async verifyPostSignature(post: WeiboPost): Promise<boolean> {
    if (!post.signature) {
      return false;
    }
    const expected = buildPostSignPayload(post.orgId, post.id, post.authorRootId, post.content);
    if (post.signature.payload !== expected) {
      // 随帖 payload 与帖子当前内容/作者/归属不符：拒绝进入验签
      return false;
    }
    const result = await this.sdk.identity.verify(expected, post.signature.signature, post.signature.publicKey);
    return result.valid;
  }

  async createComment(orgId: string, postId: string, rootId: string, content: string, parentCommentId?: string): Promise<WeiboComment> {
    await this.ensureCollectionsDeclared();
    const comment: WeiboComment = {
      id: newId('comment'),
      orgId,
      postId,
      parentCommentId,
      content: normalizeWeiboText(content),
      authorRootId: rootId,
      createdAt: Date.now()
    };

    await this.sdk.docs.put(WEIBO_COLLECTIONS.comments, comment.id, comment as unknown as Record<string, unknown>);
    return comment;
  }

  async loadPosts(orgId: string): Promise<WeiboPost[]> {
    const response = await this.sdk.docs.query<WeiboPost>(WEIBO_COLLECTIONS.posts, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: true,
      limit: 500
    });

    return response.items.map((item) => item.data).sort((a, b) => b.createdAt - a.createdAt);
  }

  async loadComments(orgId: string): Promise<WeiboComment[]> {
    const response = await this.sdk.docs.query<WeiboComment>(WEIBO_COLLECTIONS.comments, {
      filter: [{ field: 'orgId', value: orgId }],
      reverse: false,
      limit: 2000
    });

    return response.items.map((item) => item.data).sort((a, b) => a.createdAt - b.createdAt);
  }
}
