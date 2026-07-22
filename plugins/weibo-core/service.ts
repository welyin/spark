import type { PluginSDK } from '../../desktop/src/plugin-sdk-browser';
import { normalizeWeiboText, type WeiboComment, type WeiboPost } from './model';

export const WEIBO_COLLECTIONS = {
  orgConfig: 'weibo_org_config',
  posts: 'weibo_posts',
  comments: 'weibo_comments'
} as const;

/**
 * 集合同步策略声明（设计文档 V2 §4.3.4，写入前必须声明）：
 * - orgConfig：组织级配置状态，可被后续管理员调整覆盖，显式声明 lww
 * - posts / comments：内容记录，仅追加、不覆盖、不删除，使用默认 append-only（自动链式存证）
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

export class WeiboCoreService {
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

    await this.sdk.docs.put(WEIBO_COLLECTIONS.posts, post.id, post as unknown as Record<string, unknown>);
    return post;
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
