<template>
  <section class="weibo-core">
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
          <p class="eyebrow">基础插件</p>
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
        description="你还没有加入绑定“组织微博基础插件”的组织。"
      />

      <div v-if="activeOrg" class="meta-row">
        <el-tag type="info">当前 RootID: {{ currentRootId || '-' }}</el-tag>
        <el-tag :type="canPost ? 'danger' : 'warning'">
          {{ canPost ? '组织管理员' : '组织成员' }}
        </el-tag>
      </div>
    </el-card>

    <el-card v-if="activeOrg" shadow="never" class="post-card">
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
    </el-card>

    <el-card v-if="activeOrg" shadow="never">
      <template #header>
        <div class="header-row">
          <h3>时间线</h3>
          <span>{{ posts.length }} 条</span>
        </div>
      </template>

      <el-empty v-if="posts.length === 0" description="暂无短文" />

      <div v-for="post in posts" :key="post.id" class="post-item">
        <div class="post-meta">
          <strong>{{ post.authorRootId }}</strong>
          <span>{{ formatDate(post.createdAt) }}</span>
        </div>
        <p class="post-content">{{ post.content }}</p>

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
      </div>
    </el-card>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { initializePluginSDK } from '../../app/src/plugin-sdk-browser';
import type { PluginSDK } from '../../app/src/plugin-sdk-browser';
import { buildCommentThread, canPublishPost, validateWeiboText, type WeiboCommentNode } from './model';
import { WeiboCoreService } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  description: string;
  basePluginDomain?: string;
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

type PostDoc = {
  id: string;
  orgId: string;
  content: string;
  authorRootId: string;
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

export default defineComponent({
  name: 'WeiboCoreView',
  props: {
    pluginContext: {
      type: Object as () => { orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<WeiboCoreService | null>(null);
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

    const posts = ref<PostDoc[]>([]);
    const comments = ref<CommentDoc[]>([]);

    const postDraft = ref('');
    const commentDraftByPost = ref<Record<string, string>>({});
    const replyDraftByComment = ref<Record<string, string>>({});

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
        sdk.value = await initializePluginSDK();
        service.value = new WeiboCoreService(sdk.value);
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
      const domain = plugin.domain;

      orgOptions.value = all.filter((org) => org.basePluginDomain === domain) as OrganizationView[];

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
        await service.value.createPost(selectedOrgId.value, currentRootId.value, postDraft.value, currentOrgRole.value);
        postDraft.value = '';
        await loadTimeline();
        setMessage('短文发布成功（已进入插件域数据并触发P2P同步）', 'success');
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

    const commentThreadsByPost = (postId: string): WeiboCommentNode[] => {
      return buildCommentThread(postId, comments.value);
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
      void reloadAll();
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
      reloadAll,
      submitPost,
      submitComment,
      submitReply,
      commentThreadsByPost,
      formatDate
    };
  }
});
</script>

<style scoped>
.weibo-core {
  display: grid;
  gap: 14px;
}

.header-card,
.post-card {
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
