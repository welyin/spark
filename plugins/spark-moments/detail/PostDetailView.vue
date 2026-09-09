<!--
  朋友圈插件（spark-moments）· 动态详情页（detail.md）。
  完整内容（不折叠）+ 九宫格 + 绝对时间 + 删除入口 + 点赞人列表 + 评论区 + 底部操作栏。
  已删除/不可见 → 占位「该动态已删除或不可见」+ 返回。
-->
<template>
  <section class="detail">
    <header class="top">
      <button type="button" class="back" @click="pop">‹ 返回</button>
      <span class="title">动态</span>
      <span class="placeholder"></span>
    </header>

    <div v-if="notFound" class="gone">
      <p>该动态已删除或不可见</p>
      <el-button type="primary" @click="pop">返回</el-button>
    </div>

    <template v-else-if="post">
      <div class="post-body">
        <div class="author-row">
          <img v-if="authorAvatar" :src="authorAvatar" class="av" alt="" />
          <span v-else class="av av-fallback">{{ authorName.slice(0, 1) }}</span>
          <div>
            <div class="author-name">{{ authorName }}</div>
            <div class="abs-time">{{ formatAbsoluteTime(post.createdAt) }}</div>
          </div>
          <el-button v-if="isSelf" type="danger" text size="small" @click="confirmDelete">删除</el-button>
        </div>
        <p v-if="post.text" class="full-text">{{ post.text }}</p>
        <NineGrid v-if="post.images.length > 0" :images="post.images" @preview="previewIndex = $event" />
      </div>

      <div class="likes-section">
        <div class="section-title">♡ 点赞（{{ likers.length }}）</div>
        <div class="liker-list">
          <div v-for="id in likers" :key="id" class="liker">
            <span class="liker-avatar">{{ nameOf(id).slice(0, 1) }}</span>
            <span class="liker-name">{{ nameOf(id) }}</span>
          </div>
        </div>
      </div>

      <div class="comments-section">
        <div class="section-title">评论（{{ comments.length }}）</div>
        <div
          v-for="c in comments"
          :key="c.key"
          class="comment"
          @contextmenu.prevent="openCommentMenu(c)"
          @longpress.prevent="openCommentMenu(c)"
        >
          <span class="c-avatar">{{ nameOf(c.rootId).slice(0, 1) }}</span>
          <div class="c-body">
            <span class="c-name">{{ nameOf(c.rootId) }}</span>
            <span v-if="c.rootId === myRootId" class="badge">我</span>
            <span v-if="c.rootId === post.authorRootId" class="badge">作者</span>
            <p class="c-text">{{ c.interaction.text }}</p>
            <span class="c-time">{{ formatRelativeTime(c.interaction.ts) }}</span>
          </div>
        </div>
      </div>

      <div class="bottom-bar">
        <button type="button" class="action" :class="{ liked: hasLiked }" @click="likeOrCancel">
          {{ hasLiked ? '取消' : '♡ 赞' }}
        </button>
        <button type="button" class="action" @click="commenting = !commenting">💬 评论</button>
      </div>
      <div v-if="commenting" class="comment-bar">
        <input v-model="draft" maxlength="500" placeholder="说点什么…" @keyup.enter="submitComment" />
        <button :disabled="!draft.trim()" @click="submitComment">发送</button>
      </div>

      <LightboxPreview
        v-if="previewIndex >= 0"
        :images="post.images"
        :start="previewIndex"
        @close="previewIndex = -1"
      />
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { ensurePluginSDK } from '../../../packages/plugin-sdk/src';
import { usePageStack } from '../composables/usePageStack';
import { useMoments } from '../composables/useMoments';
import NineGrid from '../components/NineGrid.vue';
import LightboxPreview from '../components/LightboxPreview.vue';
import { formatAbsoluteTime, formatRelativeTime, type MomentsPost } from '../model';
import type { InteractionEntry } from '../composables/useMoments';

export default defineComponent({
  name: 'PostDetailView',
  components: { NineGrid, LightboxPreview },
  props: {
    postId: { type: String, required: true }
  },
  setup(props) {
    const { pop } = usePageStack();
    const moments = useMoments();

    const post = ref<MomentsPost | null>(null);
    const notFound = ref(false);
    const likers = ref<string[]>([]);
    const comments = ref<InteractionEntry[]>([]);
    const friends = ref<any[]>([]);
    const myRootId = ref<string | null>(null);
    const previewIndex = ref(-1);
    const commenting = ref(false);
    const draft = ref('');

    const authorName = computed(() => {
      const f = friends.value.find((x) => x.rootId === post.value?.authorRootId);
      if (f) return f.nickname;
      return post.value?.authorSnapshot?.nickname || post.value?.authorRootId.slice(0, 8) || '';
    });
    const authorAvatar = computed(() => {
      const f = friends.value.find((x) => x.rootId === post.value?.authorRootId);
      return f?.avatar || post.value?.authorSnapshot?.avatar || '';
    });
    const isSelf = computed(() => post.value?.authorRootId === myRootId.value);
    const hasLiked = computed(() => post.value && likers.value.includes(myRootId.value ?? ''));

    const nameOf = (rootId: string) => {
      const f = friends.value.find((x) => x.rootId === rootId);
      if (f) return f.nickname;
      return rootId.slice(0, 8);
    };

    onMounted(async () => {
      try {
        const sdk = await ensurePluginSDK();
        const identity = await sdk.runtime.currentRoot();
        myRootId.value = identity.rootId;
        const c = await moments.loadContacts();
        friends.value = c.friends;
      } catch { /* 无 SDK 降级 */ }

      const p = await moments.getPost(props.postId);
      if (!p || p.deletedAt != null) {
        notFound.value = true;
        return;
      }
      post.value = p;
      likers.value = moments.likersByPost.value[p.id] ?? [];
      comments.value = moments.commentsByPost.value[p.id] ?? [];
    });

    const likeOrCancel = async () => {
      if (!post.value) return;
      try {
        await moments.interact(post.value, 'like', hasLiked.value ? 'remove' : 'add');
        likers.value = moments.likersByPost.value[post.value.id] ?? [];
      } catch (error) {
        ElMessage.warning(`操作失败：${error}`);
      }
    };

    const submitComment = async () => {
      const text = draft.value.trim();
      if (!text || !post.value) return;
      try {
        await moments.interact(post.value, 'comment', 'add', text);
        draft.value = '';
        commenting.value = false;
        comments.value = moments.commentsByPost.value[post.value.id] ?? [];
      } catch (error) {
        ElMessage.warning(`评论失败：${error}`);
      }
    };

    const confirmDelete = async () => {
      if (!post.value) return;
      try {
        await ElMessageBox.confirm('删除后，这条动态将从所有联系人的设备上删除。', '删除动态', {
          type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消'
        });
        await moments.deletePost(post.value);
        ElMessage.success('已删除');
        pop();
      } catch { /* 取消 */ }
    };

    const openCommentMenu = async (comment: InteractionEntry) => {
      const canDelete = comment.rootId === myRootId.value || isSelf.value;
      if (!canDelete || !post.value) return;
      try {
        await ElMessageBox.confirm('删除这条评论？', '删除评论', {
          type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消'
        });
        await moments.deleteComment(post.value, comment.rootId);
        comments.value = moments.commentsByPost.value[post.value.id] ?? [];
      } catch { /* 取消 */ }
    };

    return {
      post, notFound, likers, comments, friends, myRootId, previewIndex, commenting, draft,
      authorName, authorAvatar, isSelf, hasLiked, nameOf, pop,
      formatAbsoluteTime, formatRelativeTime, likeOrCancel, submitComment, confirmDelete, openCommentMenu
    };
  }
});
</script>

<style scoped>
/* 页栈子页根 = iframe 内独立滚动容器（父级 .moments-root overflow:hidden 不滚动；
   长评论区在窗口最小高 220 下必超高，须自滚动）。padding-bottom 为 fixed 底栏留位 */
.detail { height: 100%; overflow-y: auto; background: var(--spark-bg-page, #f5f5f5); padding-bottom: 72px; }
.top { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); position: sticky; top: 0; z-index: 5; }
.back { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 16px; cursor: pointer; }
.title { font-weight: 600; }
.placeholder { width: 40px; }
.gone { text-align: center; padding: 80px 20px; color: var(--spark-text-2, #475569); }
.gone p { margin-bottom: 16px; }
.post-body { background: var(--spark-bg-card, #fff); padding: 16px; }
.author-row { display: flex; align-items: center; gap: 12px; }
.av { width: 44px; height: 44px; border-radius: 50%; object-fit: cover; display: flex; align-items: center; justify-content: center; background: #e2e8f0; color: #475569; font-size: 18px; }
.author-name { color: var(--spark-primary, #4f7cff); font-weight: 600; }
.abs-time { color: var(--spark-text-3, #94a3b8); font-size: 12px; margin-top: 2px; }
.full-text { margin: 16px 0; font-size: 15px; line-height: 1.6; white-space: pre-wrap; word-break: break-word; }
.likes-section, .comments-section { background: var(--spark-bg-card, #fff); margin-top: 12px; padding: 14px 16px; }
.section-title { font-weight: 600; margin-bottom: 10px; font-size: 14px; }
.liker-list { display: flex; flex-wrap: wrap; gap: 12px; }
.liker { display: flex; flex-direction: column; align-items: center; gap: 4px; width: 48px; }
.liker-avatar { width: 36px; height: 36px; border-radius: 50%; background: #e2e8f0; display: flex; align-items: center; justify-content: center; color: #475569; }
.liker-name { font-size: 11px; text-align: center; overflow: hidden; text-overflow: ellipsis; width: 100%; }
.comment { display: flex; gap: 10px; padding: 10px 0; border-bottom: 1px solid var(--spark-border, #f1f5f9); }
.c-avatar { width: 36px; height: 36px; border-radius: 50%; background: #e2e8f0; display: flex; align-items: center; justify-content: center; color: #475569; flex-shrink: 0; }
.c-body { flex: 1; min-width: 0; }
.c-name { color: var(--spark-primary, #4f7cff); font-weight: 500; }
.badge { display: inline-block; margin-left: 6px; background: var(--spark-bg-hover, #f1f5f9); color: var(--spark-text-3, #94a3b8); font-size: 11px; border-radius: 3px; padding: 0 5px; }
.c-text { margin: 4px 0; font-size: 14px; }
.c-time { color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.bottom-bar { position: fixed; bottom: 0; left: 0; right: 0; display: flex; background: var(--spark-bg-card, #fff); border-top: 1px solid var(--spark-border, #f1f5f9); padding-bottom: env(safe-area-inset-bottom); }
.action { flex: 1; border: none; background: none; padding: 14px; font-size: 15px; cursor: pointer; color: var(--spark-text-2, #475569); }
.action.liked { color: var(--spark-primary, #4f7cff); }
.comment-bar { position: fixed; bottom: 0; left: 0; right: 0; display: flex; gap: 8px; padding: 10px 12px; background: var(--spark-bg-card, #fff); border-top: 1px solid var(--spark-border, #f1f5f9); padding-bottom: calc(10px + env(safe-area-inset-bottom)); }
.comment-bar input { flex: 1; border: 1px solid var(--spark-border, #e2e8f0); border-radius: 6px; padding: 8px 10px; }
.comment-bar button { border: none; border-radius: 6px; background: var(--spark-primary, #4f7cff); color: #fff; padding: 0 16px; cursor: pointer; }
.comment-bar button:disabled { opacity: 0.5; }
</style>
