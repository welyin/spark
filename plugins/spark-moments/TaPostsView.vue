<!--
  朋友圈插件（spark-moments）· ta 的动态页（timeline.md §7.2，P1）。
  顶部简版作者卡 + 该作者发给我且我可见的动态倒序列表。
  空态为中性文案「暂无可见动态」（不区分对方没发过/没发给你看，避免泄露可见性）。
-->
<template>
  <section class="ta-posts">
    <header class="top">
      <button type="button" class="back" @click="pop">‹ 返回</button>
      <span class="title">TA 的动态</span>
      <span class="placeholder"></span>
    </header>

    <div class="author-card">
      <span class="avatar">{{ authorName.slice(0, 1) }}</span>
      <span class="name">{{ authorName }}</span>
    </div>

    <div v-if="posts.length === 0" class="empty">
      <el-empty description="暂无可见动态" />
    </div>

    <div v-else class="list">
      <TimelineItem
        v-for="post in posts"
        :key="post.id"
        :post="post"
        :likers="moments.likersByPost.value[post.id] ?? []"
        :comments="moments.commentsByPost.value[post.id] ?? []"
        :is-self="false"
        :friends="contacts.friends"
        @open-detail="openDetail"
        @open-person="switchAuthor"
        @interact="handleInteract"
        @delete-comment="handleDeleteComment"
      />
    </div>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { usePageStack } from './composables/usePageStack';
import { useMoments } from './composables/useMoments';
import TimelineItem from './components/TimelineItem.vue';
import type { MomentsPost } from './model';

export default defineComponent({
  name: 'TaPostsView',
  components: { TimelineItem },
  props: {
    authorRootId: { type: String, required: true }
  },
  setup(props) {
    const { pop, push } = usePageStack();
    const moments = useMoments();
    const contacts = ref<{ friends: any[] }>({ friends: [] });
    const posts = ref<MomentsPost[]>([]);

    const authorName = computed(() => {
      const f = contacts.value.friends.find((x) => x.rootId === props.authorRootId);
      return f ? f.nickname : props.authorRootId.slice(0, 8);
    });

    onMounted(async () => {
      contacts.value = await moments.loadContacts();
      posts.value = await moments.loadPostsByAuthor(props.authorRootId);
    });

    const openDetail = (postId: string) => {
      void import('./detail/PostDetailView.vue').then((m) => {
        push({ name: 'detail', component: m.default, props: { postId }, title: '动态' });
      });
    };
    const switchAuthor = (authorRootId: string) => {
      // 直接压一个新页帧展示该作者（props 只读，不做原地切换）
      void import('./TaPostsView.vue').then((m) => {
        push({ name: 'ta-posts', component: m.default, props: { authorRootId }, title: 'TA 的动态' });
      });
    };
    const handleInteract = async (payload: { post: MomentsPost; type: 'like' | 'comment'; action: 'add' | 'remove'; text?: string }) => {
      try { await moments.interact(payload.post, payload.type, payload.action, payload.text); }
      catch (error) { ElMessage.warning(`操作失败：${error}`); }
    };
    const handleDeleteComment = async (payload: { post: MomentsPost; commentRootId: string }) => {
      try { await moments.deleteComment(payload.post, payload.commentRootId); }
      catch (error) { ElMessage.warning(`删除失败：${error}`); }
    };

    return { pop, posts, contacts, moments, authorName, openDetail, switchAuthor, handleInteract, handleDeleteComment };
  }
});
</script>

<style scoped>
/* 页栈子页根 = iframe 内独立滚动容器：父级 .moments-root overflow:hidden 不滚动，
   min-height:100% 下超高内容会被裁剪且无处滚动（窗口最小高 220 下必现），
   故定高 100% + 自滚动（.top sticky 随之生效） */
.ta-posts { height: 100%; overflow-y: auto; background: var(--spark-bg-page, #f5f5f5); }
.top { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); position: sticky; top: 0; z-index: 5; }
.back { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 16px; cursor: pointer; }
.title { font-weight: 600; }
.placeholder { width: 40px; }
.author-card { display: flex; align-items: center; gap: 12px; padding: 20px 16px; background: var(--spark-bg-card, #fff); }
.avatar { width: 48px; height: 48px; border-radius: 50%; background: var(--spark-primary, #4f7cff); color: #fff; display: flex; align-items: center; justify-content: center; font-size: 20px; }
.name { font-size: 16px; font-weight: 600; }
.empty { padding: 40px 0; }
.list { max-width: 640px; margin: 0 auto; padding: 12px; display: grid; gap: 12px; }
</style>
