<!--
  朋友圈插件（spark-moments）· 单条动态（timeline.md §4）。
  结构：头像 + 作者昵称 / 文字（超 8 行折叠）/ 九宫格 / 时间 + 操作钮 /
  点赞摘要 / 评论摘要。交互经 emit 上报，不直接碰 SDK。
  删除手势：长按（移动端）/右键（桌面）评论行 → 删除菜单（作者可删 / 评论者可删）。
-->
<template>
  <article class="timeline-item">
    <div class="item-main">
      <button type="button" class="author-avatar" @click="$emit('open-person', post.authorRootId)">
        <img v-if="authorAvatar" :src="authorAvatar" class="av" alt="" />
        <span v-else class="av av-fallback">{{ authorName.slice(0, 1) }}</span>
      </button>
      <div class="item-body">
        <div class="author-name" @click="$emit('open-person', post.authorRootId)">{{ authorName }}</div>
        <p v-if="post.text" class="text" :class="{ collapsed: !expanded }" @click="toggleExpand">
          {{ post.text }}
        </p>
        <span v-if="post.text && post.text.length > EXPAND_THRESHOLD" class="expand-btn" @click="toggleExpand">
          {{ expanded ? '收起' : '全文' }}
        </span>
        <NineGrid v-if="post.images.length > 0" :images="post.images" @preview="openPreview" />
        <div class="meta">
          <span class="time">{{ formatRelativeTime(post.createdAt) }}</span>
          <span class="ops" @click.stop="toggleMenu">
            <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
              <circle cx="12" cy="5" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="12" cy="19" r="1.8" />
            </svg>
          </span>
          <!-- 操作气泡菜单：从「...」按钮左侧弹出，底部与按钮底部齐平（绝对定位在卡片内） -->
          <div v-if="menuOpen" class="op-menu">
            <button class="menu-item" @click="likeOrCancel">{{ hasLiked ? '取消赞' : '赞' }}</button>
            <button class="menu-item" @click="openCommentInput">评论</button>
            <button v-if="isSelf" class="menu-item danger" @click="confirmDelete">删除</button>
          </div>
        </div>
        <div class="summary-bar" v-if="likers.length > 0 || comments.length > 0">
          <div v-if="likers.length > 0" class="likes">♡ {{ likerText }}</div>
          <div
            v-for="c in shownComments"
            :key="c.key"
            class="comment-line"
            @contextmenu.prevent="openCommentMenu(c)"
            @longpress.prevent="openCommentMenu(c)"
          >
            <span class="cname">{{ nameOf(c.rootId) }}</span>：{{ c.interaction.text }}
          </div>
          <div v-if="comments.length > 3" class="more" @click="$emit('open-detail', post.id)">全部 {{ comments.length }} 条评论</div>
        </div>
      </div>
    </div>

    <!-- 点击菜单外区域关闭 -->
    <div v-if="menuOpen" class="menu-mask" @click="toggleMenu"></div>

    <!-- 评论输入条 -->
    <div v-if="commenting" class="comment-input">
      <input v-model="draft" maxlength="500" placeholder="说点什么…" @keyup.enter="submitComment" />
      <button :disabled="!draft.trim()" @click="submitComment">发送</button>
    </div>

    <LightboxPreview
      v-if="previewIndex >= 0"
      :images="post.images"
      :start="previewIndex"
      @close="previewIndex = -1"
    />
  </article>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref } from 'vue';
import { ElMessageBox } from 'element-plus';
import { ensurePluginSDK } from '../../../packages/plugin-sdk/src';
import NineGrid from './NineGrid.vue';
import LightboxPreview from './LightboxPreview.vue';
import { formatRelativeTime, type MomentsPost } from '../model';
import type { InteractionEntry } from '../composables/useMoments';

const EXPAND_THRESHOLD = 200; // 约 8 行折叠阈值（近似字符数）

export default defineComponent({
  name: 'TimelineItem',
  components: { NineGrid, LightboxPreview },
  props: {
    post: { type: Object as () => MomentsPost, required: true },
    likers: { type: Array as () => string[], default: () => [] },
    comments: { type: Array as () => InteractionEntry[], default: () => [] },
    isSelf: { type: Boolean, default: false },
    /** 我的头像（作者是自己且无 authorSnapshot 头像时兜底） */
    selfAvatar: { type: String, default: '' },
    /** 我的昵称（作者是自己时兜底展示） */
    selfNickname: { type: String, default: '我' },
    friends: { type: Array as () => any[], default: () => [] }
  },
  emits: ['open-detail', 'open-person', 'interact', 'delete-post', 'delete-comment'],
  setup(props, { emit }) {
    const expanded = ref(false);
    const menuOpen = ref(false);
    const commenting = ref(false);
    const draft = ref('');
    const previewIndex = ref(-1);

    const authorName = computed(() => {
      const f = props.friends.find((x) => x.rootId === props.post.authorRootId);
      if (f) return f.nickname;
      // 作者是自己：通讯录不含自己，用「我的昵称」兜底（authorSnapshot 无则用 selfNickname）
      if (props.isSelf) return props.post.authorSnapshot?.nickname || props.selfNickname;
      return props.post.authorSnapshot?.nickname || props.post.authorRootId.slice(0, 8);
    });
    const authorAvatar = computed(() => {
      const f = props.friends.find((x) => x.rootId === props.post.authorRootId);
      if (f?.avatar) return f.avatar;
      if (props.post.authorSnapshot?.avatar) return props.post.authorSnapshot.avatar;
      // 作者是自己：通讯录不含自己，用「我的头像」兜底（右上角/身份头像）
      if (props.isSelf) return props.selfAvatar;
      return '';
    });
    const hasLiked = computed(() => props.likers.includes(myRootId.value ?? ''));
    const likerText = computed(() => {
      const names = props.likers.map((id) => nameOf(id));
      if (names.length <= 8) return names.join('、');
      return `${names.slice(0, 2).join('、')} 等 ${names.length} 人`;
    });
    const shownComments = computed(() => props.comments.slice(0, 3));

    const myRootId = ref<string | null>(null);
    onMounted(async () => {
      try {
        const sdk = await ensurePluginSDK();
        const identity = await sdk.runtime.currentRoot();
        myRootId.value = identity.rootId;
      } catch {
        /* 无 SDK 环境降级 */
      }
      // 滚动时关闭操作菜单（capture 捕获时间线列表的任何滚动）
      window.addEventListener('scroll', closeMenuOnScroll, true);
    });
    onBeforeUnmount(() => window.removeEventListener('scroll', closeMenuOnScroll, true));
    const closeMenuOnScroll = () => { menuOpen.value = false; };

    const nameOf = (rootId: string) => {
      const f = props.friends.find((x) => x.rootId === rootId);
      if (f) return f.nickname;
      return rootId.slice(0, 8);
    };

    const toggleExpand = () => { expanded.value = !expanded.value; };
    const toggleMenu = () => { menuOpen.value = !menuOpen.value; };

    const likeOrCancel = () => {
      emit('interact', { post: props.post, type: 'like', action: hasLiked.value ? 'remove' : 'add' });
      toggleMenu();
    };

    const openCommentInput = () => {
      commenting.value = !commenting.value;
      toggleMenu();
    };

    const submitComment = () => {
      const text = draft.value.trim();
      if (!text) return;
      emit('interact', { post: props.post, type: 'comment', action: 'add', text });
      draft.value = '';
      commenting.value = false;
    };

    const confirmDelete = async () => {
      toggleMenu();
      try {
        await ElMessageBox.confirm('删除后，这条动态将从所有联系人的设备上删除。', '删除动态', {
          type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消'
        });
        emit('delete-post', props.post);
      } catch {
        /* 取消 */
      }
    };

    const openCommentMenu = async (comment: InteractionEntry) => {
      const canDelete = comment.rootId === myRootId.value || props.isSelf;
      if (!canDelete) return;
      try {
        await ElMessageBox.confirm('删除这条评论？', '删除评论', {
          type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消'
        });
        emit('delete-comment', { post: props.post, commentRootId: comment.rootId });
      } catch {
        /* 取消 */
      }
    };

    const openPreview = (index: number) => { previewIndex.value = index; };

    return {
      expanded, menuOpen, commenting, draft, previewIndex,
      authorName, authorAvatar, hasLiked, likerText, shownComments,
      EXPAND_THRESHOLD, nameOf, myRootId,
      formatRelativeTime, toggleExpand, toggleMenu, likeOrCancel,
      openCommentInput, submitComment, confirmDelete, openCommentMenu, openPreview
    };
  }
});
</script>

<style scoped>
.timeline-item {
  background: var(--spark-bg-card, #fff);
  border-radius: 8px;
  padding: 12px;
  position: relative;
}
.item-main { display: flex; align-items: flex-start; gap: 10px; }
.author-avatar { border: none; padding: 0; margin-top: 1px; background: transparent; cursor: pointer; flex-shrink: 0; }
.av { width: 40px; height: 40px; border-radius: 50%; object-fit: cover; display: flex; align-items: center; justify-content: center; background: #e2e8f0; color: #475569; font-size: 16px; }
.item-body { flex: 1; min-width: 0; }
.author-name { color: var(--spark-primary, #4f7cff); font-size: 14px; font-weight: 600; cursor: pointer; }
.text { margin: 8px 0; font-size: 15px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; }
.text.collapsed { display: -webkit-box; -webkit-line-clamp: 8; -webkit-box-orient: vertical; overflow: hidden; }
.expand-btn { color: var(--spark-primary, #4f7cff); font-size: 13px; cursor: pointer; }
.meta { display: flex; justify-content: space-between; align-items: center; margin-top: 8px; position: relative; }
.time { color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.ops { color: var(--spark-text-3, #94a3b8); cursor: pointer; padding: 4px; }
.summary-bar { background: var(--spark-bg-hover, #f1f5f9); border-radius: 4px; padding: 8px 10px; margin-top: 8px; font-size: 13px; }
.likes { color: var(--spark-text-2, #475569); }
.comment-line { margin-top: 4px; word-break: break-word; }
.cname { color: var(--spark-primary, #4f7cff); }
.more { margin-top: 4px; color: var(--spark-primary, #4f7cff); cursor: pointer; font-size: 12px; }
.menu-mask { position: fixed; inset: 0; z-index: 20; }
.op-menu {
  /* 卡片内绝对定位：随卡片滚动；从「...」按钮左侧弹出，底部与按钮底部齐平 */
  position: absolute;
  right: 26px;
  bottom: 0;
  background: #fff;
  border-radius: 8px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15);
  z-index: 21;
  overflow: hidden;
  min-width: 100px;
}
.menu-item { display: block; width: 100%; border: none; background: none; padding: 10px 16px; text-align: left; cursor: pointer; font-size: 14px; }
.menu-item:hover { background: #f1f5f9; }
.menu-item.danger { color: var(--spark-danger, #ef4444); }
.comment-input { display: flex; gap: 8px; margin-top: 8px; }
.comment-input input { flex: 1; border: 1px solid var(--spark-border, #e2e8f0); border-radius: 6px; padding: 6px 10px; font-size: 14px; }
.comment-input button { border: none; border-radius: 6px; background: var(--spark-primary, #4f7cff); color: #fff; padding: 0 14px; cursor: pointer; }
</style>
