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
 *   4) messages（message:app）：发帖后向组织应用会话发通知 + 帖子卡片。
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
    const payload = buildPostSignPayload(post.orgId, post.id, post.content);
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
   * SDK 用法：sdk.messages.sendAppMessage(payload, card)——
   * - payload.summary 是强制的声明式摘要：未装插件的成员设备上壳层原生
   *   渲染这段纯文本，必须自成一体（buildPostSummary 已保证）；
   * - card={viewId:'post-card', data:{postId}} 挂了插件的设备用
   *   message-card 视图富渲染；data 只放引用（postId），正文经 docs
   *   查询——卡片数据随应用消息本地落库，放正文会冗余两份且无法同步更新。
   *
   * 降级：权限被拒/内核限流（10 条/60s）时不阻断发帖，仅告警。
   */
  async notifyNewPost(post: WeiboPost): Promise<void> {
    if (!this.sdk.messages) {
      // tab 同进程模式无 messages 模块（SDK 契约上为可选字段）
      return;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: buildPostSummary(post.content), postId: post.id, orgId: post.orgId },
        { viewId: 'post-card', data: { postId: post.id } }
      );
    } catch (error) {
      console.warn('[spark-example] 应用消息发送失败（权限/限流降级）：', error);
    }
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
   */
  async verifyPostSignature(post: WeiboPost): Promise<boolean> {
    if (!post.signature) {
      return false;
    }
    const { payload, signature, publicKey } = post.signature;
    const result = await this.sdk.identity.verify(payload, signature, publicKey);
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
