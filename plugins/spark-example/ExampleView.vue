<!--
  示例插件（spark-example）· 主视图（app 视图）：组织微博时间线。

  本文件是插件体系的参考实现，集中演示：
  - runtime.currentRoot / listMineOrganizations / syncOrganizationData（org:read、org:sync）；
  - docs 读写（经 service 层，storage:read/write；集合名 weibo_* 为数据键，跨版本稳定）；
  - identity:sign 发帖防抵赖 + identity.verify 免权限验签（「已签名」徽标）；
  - messages.sendAppMessage 发帖后通知组织应用会话（message:app）；
  - messages.onCardAction 接收消息卡片「去评论」回调 → 定位该帖并展开评论区。
-->
<template>
  <section class="spark-example">
    <el-alert
      v-if="message"
      :title="message"
      :type="messageType"
      :closable="false"
      show-icon
      class="message"
    />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">示例插件</p>
          <h2>组织微博</h2>
          <p class="lede">组织管理员可发布 260 字以内短文，组织成员可评论与回复。</p>
        </div>
        <el-button @click="reloadAll" :loading="loading">刷新</el-button>
      </div>

      <el-form label-position="top" class="selectors" v-if="orgOptions.length > 0">
        <el-form-item label="组织">
          <el-select v-model="selectedOrgId" @change="reloadAll" placeholder="选择组织">
            <el-option
              v-for="org in orgOptions"
              :key="org.orgId"
              :label="`${org.name} (${org.orgId.slice(0, 8)}...)`"
              :value="org.orgId"
            />
          </el-select>
        </el-form-item>
      </el-form>

      <el-empty
        v-if="orgOptions.length === 0"
        description="你还没有加入任何组织。"
      />

      <div v-if="activeOrg" class="meta-row">
        <el-tag type="info">当前 RootID: {{ currentRootId || '-' }}</el-tag>
        <el-tag :type="canPost ? 'danger' : 'warning'">
          {{ canPost ? '组织管理员' : '组织成员' }}
        </el-tag>
      </div>
    </el-card>

    <el-card v-if="activeOrg" shadow="never" class="composer-card">
      <template #header>
        <div class="header-row">
          <h3>发布短文</h3>
          <span class="counter">{{ postDraft.length }}/260</span>
        </div>
      </template>

      <el-input
        v-model="postDraft"
        type="textarea"
        :rows="3"
        maxlength="260"
        show-word-limit
        placeholder="输入短文（最多260字）"
      />
      <div class="actions">
        <el-button type="primary" :disabled="!canPost" :loading="posting" @click="submitPost">
          发送短文
        </el-button>
      </div>
      <p class="hint" v-if="!canPost">只有组织管理员可以发布短文。</p>
      <p class="hint" v-else>发帖将请求一次域身份签名（防抵赖），并向组织应用会话发送新帖通知。</p>
    </el-card>

    <el-card v-if="activeOrg" shadow="never">
      <template #header>
        <div class="header-row">
          <h3>时间线</h3>
          <span>{{ posts.length }} 条</span>
        </div>
      </template>

      <el-empty v-if="posts.length === 0" description="暂无短文" />

      <div
        v-for="post in posts"
        :key="post.id"
        :id="`post-${post.id}`"
        class="post-item"
        :class="{ highlighted: highlightedPostId === post.id }"
      >
        <div class="post-meta">
          <strong>{{ post.authorRootId }}</strong>
          <span>{{ formatDate(post.createdAt) }}</span>
        </div>
        <p class="post-content">{{ post.content }}</p>

        <div class="post-flags">
          <!-- identity:sign 演示：签名随帖存储，任何成员可免权限验签 -->
          <el-tag v-if="post.signature" type="success" size="small">已签名</el-tag>
          <el-button
            v-if="post.signature"
            size="small"
            text
            :loading="verifyingPostId === post.id"
            @click="toggleVerify(post)"
          >
            验签
          </el-button>
          <span v-if="verifyResultByPost[post.id]" class="verify-result">
            {{ verifyResultByPost[post.id] }}
          </span>
        </div>

        <div class="comment-toggle">
          <el-button size="small" text @click="toggleComments(post.id)">
            {{ expandedPostIds[post.id] ? '收起评论' : `评论（${commentCountByPost(post.id)}）` }}
          </el-button>
        </div>

        <template v-if="expandedPostIds[post.id]">
          <div class="reply-editor">
            <el-input
              v-model="commentDraftByPost[post.id]"
              maxlength="260"
              show-word-limit
              placeholder="发表评论"
            />
            <el-button size="small" type="primary" :loading="commentingPostId === post.id" @click="submitComment(post.id)">
              评论
            </el-button>
          </div>

          <div class="comment-list">
            <div v-for="node in commentThreadsByPost(post.id)" :key="node.comment.id" class="comment-item">
              <div class="post-meta">
                <strong>{{ node.comment.authorRootId }}</strong>
                <span>{{ formatDate(node.comment.createdAt) }}</span>
              </div>
              <p class="comment-content">
                {{ node.comment.content }}
              </p>
              <div class="reply-editor small">
                <el-input
                  v-model="replyDraftByComment[node.comment.id]"
                  maxlength="260"
                  show-word-limit
                  placeholder="回复评论"
                />
                <el-button
                  size="small"
                  :loading="commentingCommentId === node.comment.id"
                  @click="submitReply(post.id, node.comment.id)"
                >
                  回复
                </el-button>
              </div>

              <div
                v-for="reply in node.replies"
                :key="reply.id"
                class="comment-item nested"
              >
                <div class="post-meta">
                  <strong>{{ reply.authorRootId }}</strong>
                  <span>{{ formatDate(reply.createdAt) }}</span>
                </div>
                <p class="comment-content">
                  <span class="reply-flag">回复：</span>{{ reply.content }}
                </p>
              </div>
            </div>
          </div>
        </template>
      </div>
    </el-card>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginCardActionPayload, PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildCommentThread,
  canPublishPost,
  validateWeiboText,
  type WeiboCommentNode,
  type WeiboPost
} from './model';
import { WeiboService } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  description: string;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    nodeInfo?: {
      peerId?: string;
      addresses: string[];
    };
  }>;
};

type OrgConfigDoc = {
  orgId: string;
  superAdminRootId: string;
  createdBy: string;
  createdAt: number;
};

type CommentDoc = {
  id: string;
  orgId: string;
  postId: string;
  parentCommentId?: string;
  content: string;
  authorRootId: string;
  createdAt: number;
};

/** 卡片回调后高亮时长（ms）：足够用户注意到定位目标，又不永久占用视觉焦点 */
const HIGHLIGHT_DURATION_MS = 2500;

export default defineComponent({
  name: 'ExampleView',
  props: {
    pluginContext: {
      type: Object as () => { orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<WeiboService | null>(null);
    const loading = ref(false);
    const posting = ref(false);
    const commentingPostId = ref('');
    const commentingCommentId = ref('');
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');
    const orgConfig = ref<OrgConfigDoc | null>(null);

    const posts = ref<WeiboPost[]>([]);
    const comments = ref<CommentDoc[]>([]);

    const postDraft = ref('');
    const commentDraftByPost = ref<Record<string, string>>({});
    const replyDraftByComment = ref<Record<string, string>>({});

    // 评论区展开状态（默认收起，卡片回调「去评论」会展开目标帖）
    const expandedPostIds = ref<Record<string, boolean>>({});
    const highlightedPostId = ref('');
    // 验签结果按帖缓存（避免重复点击重复验签）
    const verifyingPostId = ref('');
    const verifyResultByPost = ref<Record<string, string>>({});

    let highlightTimer: ReturnType<typeof setTimeout> | null = null;
    let offCardAction: (() => void) | null = null;

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const canPost = computed(() => canPublishPost(currentOrgRole.value));

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureSdk = async () => {
      if (!sdk.value) {
        // SDK 由插件入口在桥握手完成时注入 window.__sparkPluginSDK，
        // 视图挂载可能先于握手完成，挂起等待注入
        sdk.value = await ensurePluginSDK();
        service.value = new WeiboService(sdk.value);
      }
      return sdk.value;
    };

    const ensureOrgConfig = async (orgId: string): Promise<OrgConfigDoc> => {
      await ensureSdk();
      if (!service.value) {
        throw new Error('Plugin service unavailable');
      }

      if (!currentRootId.value) {
        throw new Error('Root identity is locked');
      }

      return await service.value.ensureOrgConfig(orgId, currentRootId.value);
    };

    const loadOrganizations = async () => {
      const plugin = await ensureSdk();
      const all = await plugin.runtime.listMineOrganizations();

      // 组织与插件无绑定（basePluginDomain 已删除）：全部已加入组织皆可选
      orgOptions.value = all as OrganizationView[];

      const preferredOrgId = props.pluginContext?.orgId;
      if (preferredOrgId && orgOptions.value.some((org) => org.orgId === preferredOrgId)) {
        selectedOrgId.value = preferredOrgId;
        return;
      }

      if (!orgOptions.value.some((org) => org.orgId === selectedOrgId.value)) {
        selectedOrgId.value = orgOptions.value[0]?.orgId ?? '';
      }
    };

    const loadTimeline = async () => {
      await ensureSdk();
      if (!service.value) {
        throw new Error('Plugin service unavailable');
      }
      if (!selectedOrgId.value) {
        posts.value = [];
        comments.value = [];
        orgConfig.value = null;
        return;
      }

      orgConfig.value = await ensureOrgConfig(selectedOrgId.value);

      posts.value = await service.value.loadPosts(selectedOrgId.value);
      comments.value = await service.value.loadComments(selectedOrgId.value);
    };

    const syncLatestFromPeers = async () => {
      if (!selectedOrgId.value) {
        return;
      }

      const plugin = await ensureSdk();
      try {
        await plugin.runtime.syncOrganizationData(selectedOrgId.value);
      } catch (error) {
        setMessage(`成员数据同步失败：${error}`, 'warning');
      }
    };

    const reloadAll = async () => {
      loading.value = true;
      try {
        const plugin = await ensureSdk();
        const identity = await plugin.runtime.currentRoot();
        currentRootId.value = identity.rootId;

        await loadOrganizations();
        await syncLatestFromPeers();
        await loadTimeline();
      } catch (error) {
        setMessage(`加载失败：${error}`, 'error');
      } finally {
        loading.value = false;
      }
    };

    const submitPost = async () => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      const validation = validateWeiboText(postDraft.value);
      if (!validation.ok) {
        ElMessage.warning(validation.reason || '短文内容不合法');
        return;
      }
      if (!canPost.value) {
        ElMessage.warning('只有组织管理员可以发帖');
        return;
      }

      posting.value = true;
      try {
        await ensureSdk();
        if (!service.value) {
          throw new Error('Plugin service unavailable');
        }
        // createPost 内部先 identity:sign 签名（用户拒绝则降级为无签名）
        const post = await service.value.createPost(
          selectedOrgId.value,
          currentRootId.value,
          postDraft.value,
          currentOrgRole.value
        );
        postDraft.value = '';
        // 发帖成功 → 向组织应用会话发新帖通知（含 post-card 卡片），失败仅告警
        await service.value.notifyNewPost(post);
        await loadTimeline();
        setMessage('短文发布成功（已进入插件域数据并触发P2P同步，应用会话已收到新帖通知）', 'success');
      } catch (error) {
        setMessage(`发布失败：${error}`, 'error');
      } finally {
        posting.value = false;
      }
    };

    const submitComment = async (postId: string) => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      const raw = commentDraftByPost.value[postId] || '';
      const validation = validateWeiboText(raw);
      if (!validation.ok) {
        ElMessage.warning(validation.reason || '评论内容不合法');
        return;
      }

      commentingPostId.value = postId;
      try {
        await ensureSdk();
        if (!service.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.createComment(selectedOrgId.value, postId, currentRootId.value, raw);
        commentDraftByPost.value = {
          ...commentDraftByPost.value,
          [postId]: ''
        };
        await loadTimeline();
      } catch (error) {
        setMessage(`评论失败：${error}`, 'error');
      } finally {
        commentingPostId.value = '';
      }
    };

    const submitReply = async (postId: string, parentCommentId: string) => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      const raw = replyDraftByComment.value[parentCommentId] || '';
      const validation = validateWeiboText(raw);
      if (!validation.ok) {
        ElMessage.warning(validation.reason || '回复内容不合法');
        return;
      }

      commentingCommentId.value = parentCommentId;
      try {
        await ensureSdk();
        if (!service.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.createComment(selectedOrgId.value, postId, currentRootId.value, raw, parentCommentId);
        replyDraftByComment.value = {
          ...replyDraftByComment.value,
          [parentCommentId]: ''
        };
        await loadTimeline();
      } catch (error) {
        setMessage(`回复失败：${error}`, 'error');
      } finally {
        commentingCommentId.value = '';
      }
    };

    const toggleComments = (postId: string) => {
      expandedPostIds.value = {
        ...expandedPostIds.value,
        [postId]: !expandedPostIds.value[postId]
      };
    };

    /**
     * 卡片回调（messages.onCardAction）：消息卡片「去评论」经壳层归属校验
     * 后路由到这里。定位目标帖（不在当前时间线则先重载）→ 展开评论区 →
     * 滚动到位并短暂高亮。主实例未运行时壳层直接丢弃 action（设计允许）。
     */
    const handleCardAction = async (action: PluginCardActionPayload) => {
      if (action.actionId !== 'goto-comments') {
        return;
      }
      const postId = (action.data as { postId?: string } | undefined)?.postId;
      if (!postId) {
        return;
      }
      if (!posts.value.some((post) => post.id === postId)) {
        // 目标帖不在当前视图（可能尚未加载/同步）：重载一次再定位
        await loadTimeline().catch(() => undefined);
      }
      if (!posts.value.some((post) => post.id === postId)) {
        setMessage('目标帖子尚未同步到本机，请稍后重试。', 'warning');
        return;
      }
      expandedPostIds.value = { ...expandedPostIds.value, [postId]: true };
      await nextTick();
      document.getElementById(`post-${postId}`)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      highlightedPostId.value = postId;
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
      highlightTimer = setTimeout(() => {
        highlightedPostId.value = '';
      }, HIGHLIGHT_DURATION_MS);
    };

    /** 验签（identity.verify 免权限演示）：结果按帖缓存展示 */
    const toggleVerify = async (post: WeiboPost) => {
      await ensureSdk();
      if (!service.value) {
        return;
      }
      verifyingPostId.value = post.id;
      try {
        const valid = await service.value.verifyPostSignature(post);
        verifyResultByPost.value = {
          ...verifyResultByPost.value,
          [post.id]: valid ? '验签通过：确为作者域身份签发' : '验签失败：签名与内容不符'
        };
      } catch (error) {
        verifyResultByPost.value = { ...verifyResultByPost.value, [post.id]: `验签出错：${error}` };
      } finally {
        verifyingPostId.value = '';
      }
    };

    const commentThreadsByPost = (postId: string): WeiboCommentNode[] => {
      return buildCommentThread(postId, comments.value);
    };

    const commentCountByPost = (postId: string): number => {
      return comments.value.filter((comment) => comment.postId === postId).length;
    };

    const formatDate = (timestamp: number) => {
      return new Intl.DateTimeFormat('zh-CN', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
      }).format(new Date(timestamp));
    };

    onMounted(() => {
      void (async () => {
        const plugin = await ensureSdk();
        // 注册卡片回调（返回注销函数；仅 app 主视图注册，卡片视图收不到）
        offCardAction = plugin.messages?.onCardAction((action) => {
          void handleCardAction(action);
        }) ?? null;
        await reloadAll();
      })();
    });

    onUnmounted(() => {
      offCardAction?.();
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
    });

    watch(
      () => props.pluginContext?.orgId,
      (orgId) => {
        if (!orgId) {
          return;
        }
        if (selectedOrgId.value === orgId) {
          return;
        }
        if (!orgOptions.value.some((org) => org.orgId === orgId)) {
          return;
        }
        selectedOrgId.value = orgId;
        void loadTimeline();
      }
    );

    return {
      loading,
      posting,
      commentingPostId,
      commentingCommentId,
      message,
      messageType,
      currentRootId,
      orgOptions,
      selectedOrgId,
      activeOrg,
      canPost,
      posts,
      postDraft,
      commentDraftByPost,
      replyDraftByComment,
      expandedPostIds,
      highlightedPostId,
      verifyingPostId,
      verifyResultByPost,
      reloadAll,
      submitPost,
      submitComment,
      submitReply,
      toggleComments,
      toggleVerify,
      commentThreadsByPost,
      commentCountByPost,
      formatDate
    };
  }
});
</script>

<style scoped>
.spark-example {
  display: grid;
  gap: 14px;
}

.header-card,
.composer-card {
  border-radius: 12px;
}

.message {
  margin-bottom: 2px;
}

.header-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.eyebrow {
  margin: 0 0 6px;
  color: #0f766e;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

h2,
h3 {
  margin: 0;
}

.lede {
  margin: 8px 0 0;
  color: #64748b;
}

.selectors {
  margin-top: 12px;
}

.meta-row {
  display: flex;
  gap: 10px;
}

.counter {
  color: #64748b;
  font-size: 13px;
}

.actions {
  margin-top: 10px;
}

.hint {
  color: #64748b;
  margin: 8px 0 0;
}

.post-item {
  border: 1px solid var(--el-border-color);
  border-radius: 10px;
  padding: 12px;
  margin-bottom: 12px;
  transition: border-color 0.3s, box-shadow 0.3s;
}

/* 卡片回调定位高亮：短暂呼吸后自动消退（见 HIGHLIGHT_DURATION_MS） */
.post-item.highlighted {
  border-color: #0f766e;
  box-shadow: 0 0 0 3px rgba(15, 118, 110, 0.18);
}

.post-meta {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
}

.post-content,
.comment-content {
  margin: 8px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.post-flags {
  display: flex;
  align-items: center;
  gap: 8px;
}

.verify-result {
  color: #64748b;
  font-size: 12px;
}

.comment-toggle {
  margin-top: 4px;
}

.comment-list {
  margin-top: 10px;
  display: grid;
  gap: 8px;
}

.comment-item {
  border-left: 2px solid #d1fae5;
  background: #f8fafc;
  padding: 8px;
}

.comment-item.nested {
  margin-left: 16px;
  border-left-color: #bae6fd;
}

.reply-flag {
  color: #0f766e;
  font-weight: 600;
}

.reply-editor {
  display: flex;
  gap: 8px;
  align-items: center;
}

.reply-editor.small {
  margin-top: 6px;
}
</style>
