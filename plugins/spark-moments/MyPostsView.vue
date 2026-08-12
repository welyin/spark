<!--
  朋友圈插件（spark-moments）· 我的动态页（timeline.md §7.1）。
  仅我自己的动态，复用 TimelineItem 倒序列表；每条含「删除」。
-->
<template>
  <section class="my-posts">
    <header class="top">
      <button type="button" class="back" @click="pop">‹ 返回</button>
      <span class="title">我的动态</span>
      <span class="placeholder"></span>
    </header>

    <div v-if="posts.length === 0" class="empty">
      <el-empty description="你还没有发过动态">
        <el-button type="primary" @click="openComposer">发一条试试</el-button>
      </el-empty>
    </div>

    <div v-else class="list">
      <TimelineItem
        v-for="post in posts"
        :key="post.id"
        :post="post"
        :likers="moments.likersByPost.value[post.id] ?? []"
        :comments="moments.commentsByPost.value[post.id] ?? []"
        :is-self="true"
        :self-avatar="moments.selfProfile.value?.avatar ?? ''"
        :self-nickname="moments.selfProfile.value?.nickname ?? '我'"
        :friends="contacts.friends"
        @open-detail="openDetail"
        @open-person="openTaPosts"
        @interact="handleInteract"
        @delete-post="handleDeletePost"
        @delete-comment="handleDeleteComment"
      />
    </div>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { usePageStack } from './composables/usePageStack';
import { useMoments } from './composables/useMoments';
import TimelineItem from './components/TimelineItem.vue';
import type { MomentsPost } from './model';

export default defineComponent({
  name: 'MyPostsView',
  components: { TimelineItem },
  setup() {
    const { pop, push } = usePageStack();
    const moments = useMoments();
    const contacts = ref<{ friends: any[] }>({ friends: [] });
    const posts = ref<MomentsPost[]>([]);

    onMounted(async () => {
      posts.value = await moments.loadMyPosts();
      contacts.value = await moments.loadContacts();
    });

    const openDetail = (postId: string) => {
      void import('./detail/PostDetailView.vue').then((m) => {
        push({ name: 'detail', component: m.default, props: { postId }, title: '动态' });
      });
    };
    const openTaPosts = (authorRootId: string) => {
      void import('./TaPostsView.vue').then((m) => {
        push({ name: 'ta-posts', component: m.default, props: { authorRootId }, title: 'TA 的动态' });
      });
    };
    const openComposer = () => {
      void import('./composer/ComposerView.vue').then((m) => {
        push({ name: 'composer', component: m.default, title: '发动态' });
      });
    };

    const handleInteract = async (payload: { post: MomentsPost; type: 'like' | 'comment'; action: 'add' | 'remove'; text?: string }) => {
      try { await moments.interact(payload.post, payload.type, payload.action, payload.text); }
      catch (error) { ElMessage.warning(`操作失败：${error}`); }
    };
    const handleDeletePost = async (post: MomentsPost) => {
      try {
        await ElMessageBox.confirm('删除后，这条动态将从所有联系人的设备上删除。', '删除动态', {
          type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消'
        });
        await moments.deletePost(post);
        posts.value = posts.value.filter((p) => p.id !== post.id);
        ElMessage.success('已删除');
      } catch { /* 取消 */ }
    };
    const handleDeleteComment = async (payload: { post: MomentsPost; commentRootId: string }) => {
      try { await moments.deleteComment(payload.post, payload.commentRootId); }
      catch (error) { ElMessage.warning(`删除失败：${error}`); }
    };

    return { pop, posts, contacts, moments, openDetail, openTaPosts, openComposer, handleInteract, handleDeletePost, handleDeleteComment };
  }
});
</script>

<style scoped>
.my-posts { min-height: 100%; background: var(--spark-bg-page, #f5f5f5); }
.top { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); position: sticky; top: 0; z-index: 5; }
.back { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 16px; cursor: pointer; }
.title { font-weight: 600; }
.placeholder { width: 40px; }
.empty { padding: 40px 0; }
.list { max-width: 640px; margin: 0 auto; padding: 12px; display: grid; gap: 12px; }
</style>
