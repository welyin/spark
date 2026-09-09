<!--
  朋友圈插件（spark-moments）· 主视图（app 视图，UI 设计 timeline.md）。
  栈底页：封面头部 + 时间线流 + 相机入口；页栈容器承载子页面
  （发动态 / 谁可以看 / 名单编辑器 / 动态详情 / 我的动态）。
-->
<template>
  <section class="moments-root">
    <!-- 页栈：非空时渲染当前页帧 -->
    <component
      :is="current?.component"
      v-if="current"
      v-bind="current.props"
      :key="current.name"
      @back="pop"
    />
    <!-- 主页 -->
    <template v-else>
      <!-- 插件自接管顶栏（chrome.hostTitleBar:false）：标题「朋友圈」+ 右上角发表按钮；
           顶部固定占位（sticky），不随内容浮动，始终占据标题栏高度 -->
      <header class="moments-topbar">
        <!-- 左上角返回按钮：退出朋友圈插件。仅触屏移动形态（全屏 App）渲染——
             移动全屏下壳层沉浸式无可见返回（仅 Android 硬件返回键），此钮是唯一可见出口；
             PC 窗口模式 WindowFrame 自带关闭钮，插件内退出钮语义重复，不渲染（ui-layout） -->
        <button v-if="isTouchLayout" type="button" class="back-fab" title="返回" @click="closePlugin">
          <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M15 18l-6-6 6-6" />
          </svg>
        </button>
        <h1 class="cover-title">朋友圈</h1>
        <!-- 右上角发表按钮：发动态入口 -->
        <button type="button" class="compose-fab" title="发动态" @click="openComposer">
          <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
            <path d="M9.5 3 8 5H5a3 3 0 0 0-3 3v9a3 3 0 0 0 3 3h14a3 3 0 0 0 3-3V8a3 3 0 0 0-3-3h-3l-1.5-2h-5zM12 8.5A3.5 3.5 0 1 1 8.5 12 3.5 3.5 0 0 1 12 8.5z" />
          </svg>
        </button>
      </header>
      <div class="moments-scroll">
      <header class="cover">
        <div class="cover-bg"></div>
        <button type="button" class="avatar-btn" @click="openMyPosts">
          <img v-if="selfProfile?.avatar" :src="selfProfile.avatar" class="avatar" alt="" />
          <span v-else class="avatar avatar-fallback">{{ avatarFallback }}</span>
        </button>
        <span class="my-name">{{ selfProfile?.nickname || '我' }}</span>
      </header>

      <main class="timeline">
        <div v-if="offline" class="offline-bar">当前离线，可浏览已有动态；新动态将在联网后收发</div>

        <div v-if="!ready" class="skeleton">
          <div v-for="i in 3" :key="i" class="skeleton-item">
            <div class="sk-avatar"></div>
            <div class="sk-lines"><div class="sk-line"></div><div class="sk-line short"></div></div>
          </div>
        </div>

        <el-empty
          v-else-if="posts.length === 0"
          :image-size="90"
          description="还没有动态"
        >
          <el-button type="primary" @click="openComposer">发一条试试</el-button>
        </el-empty>

        <template v-else>
          <TimelineItem
            v-for="post in posts"
            :key="post.id"
            :post="post"
            :likers="likersByPost[post.id] ?? []"
            :comments="commentsByPost[post.id] ?? []"
            :is-self="post.authorRootId === myRootId"
            :self-avatar="selfProfile?.avatar ?? ''"
            :self-nickname="selfProfile?.nickname ?? '我'"
            :friends="contacts.friends"
            @open-detail="openDetail"
            @open-person="openTaPosts"
            @interact="handleInteract"
            @delete-post="handleDeletePost"
            @delete-comment="handleDeleteComment"
          />
          <div class="end-hint" v-if="posts.length >= pageSize">没有更多了</div>
        </template>
      </main>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { usePageStack } from './composables/usePageStack';
import { useMoments } from './composables/useMoments';
import { isTouchLayout } from './ui-layout';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import TimelineItem from './components/TimelineItem.vue';
import type { MomentsPost } from './model';

const MOMENTS_PAGE_SIZE = 20;

export default defineComponent({
  name: 'MomentsView',
  components: { TimelineItem },
  setup() {
    const { current, pop, push } = usePageStack();
    const moments = useMoments();
    const contacts = ref<{ friends: any[]; groups: any[]; tags: any[] }>({ friends: [], groups: [], tags: [] });

    const { posts, likersByPost, commentsByPost, selfProfile, myRootId, ready, offline } = moments;
    const pageSize = MOMENTS_PAGE_SIZE;

    const avatarFallback = computed(() => (selfProfile.value?.nickname || '我').slice(0, 1));

    onMounted(async () => {
      await moments.init();
      contacts.value = await moments.loadContacts();
    });

    const openComposer = () => {
      void import('./composer/ComposerView.vue').then((m) => {
        push({ name: 'composer', component: m.default, title: '发动态' });
      });
    };

    // 左上角返回（仅触屏移动形态渲染）：退出朋友圈插件（请求壳层关闭当前插件 tab/窗口）
    const closePlugin = async () => {
      try {
        const sdk = await ensurePluginSDK();
        await sdk.close();
      } catch {
        /* 无 SDK 环境（如纯浏览器预览）降级无操作 */
      }
    };

    const openMyPosts = () => {
      void import('./MyPostsView.vue').then((m) => {
        push({ name: 'my-posts', component: m.default, title: '我的动态' });
      });
    };

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

    const handleInteract = async (payload: { post: MomentsPost; type: 'like' | 'comment'; action: 'add' | 'remove'; text?: string }) => {
      try {
        await moments.interact(payload.post, payload.type, payload.action, payload.text);
      } catch (error) {
        ElMessage.warning(`操作失败：${error}`);
      }
    };

    const handleDeletePost = async (post: MomentsPost) => {
      try {
        await ElMessageBox.confirm('删除后，这条动态将从所有联系人的设备上删除。', '删除动态', {
          type: 'warning',
          confirmButtonText: '删除',
          cancelButtonText: '取消',
          confirmButtonClass: 'danger'
        });
        await moments.deletePost(post);
        ElMessage.success('已删除');
      } catch {
        /* 用户取消 */
      }
    };

    const handleDeleteComment = async (payload: { post: MomentsPost; commentRootId: string }) => {
      try {
        await moments.deleteComment(payload.post, payload.commentRootId);
      } catch (error) {
        ElMessage.warning(`删除失败：${error}`);
      }
    };

    return {
      current,
      pop,
      posts,
      likersByPost,
      commentsByPost,
      selfProfile,
      myRootId,
      ready,
      offline,
      pageSize,
      avatarFallback,
      contacts,
      isTouchLayout,
      openComposer,
      openMyPosts,
      openDetail,
      openTaPosts,
      closePlugin,
      handleInteract,
      handleDeletePost,
      handleDeleteComment
    };
  }
});
</script>

<style scoped>
.moments-root {
  /* 高度沿 html/body/#app 链取 100%（同 spark-chat/spark-minichat 先例口径；
     iframe 内等同窗口内容高）；overflow hidden 使整页不滚动，
     只有内部 .moments-scroll / 页栈子页各自独立滚动 */
  height: 100%;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page, #f5f5f5);
  color: var(--spark-text-1, #1e293b);
  font-size: 14px;
  /* 系统字体栈，与壳层保持一致（不使用特殊字体） */
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue',
    Arial, 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif;
}
.moments-scroll {
  /* 唯一滚动区域：标题栏固定不动，封面 + 时间线一起滚动 */
  flex: 1;
  overflow-y: auto;
  min-height: 0;
}
.cover {
  position: relative;
  height: 200px;
}
.cover-bg {
  position: absolute;
  inset: 0;
  background: linear-gradient(135deg, #4f7cff 0%, #7b5cff 100%);
}
/* 插件自接管顶栏：标题「朋友圈」居中 + 右上角发表按钮（壳层顶栏隐藏后由插件自己提供）。
   位于 flex 列顶部的固定条（flex-shrink:0），始终占据标题栏高度；只有时间线区域独立滚动，
   标题栏与封面不随之滚动。配色与壳层顶栏 token 一致（--spark-topbar-bg：#f8fafc + 底部
   细分隔线；若插件 iframe 读不到变量则回退到壳层浅色顶栏色 #f8fafc） */
.moments-topbar {
  position: relative; /* 作为 .compose-fab 的定位上下文，让发表按钮贴标题栏右侧 */
  flex-shrink: 0;
  height: 56px;
  padding: 0 16px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #1e293b;
  background: var(--spark-topbar-bg, #f8fafc);
  border-bottom: 1px solid var(--spark-border-light, rgba(0, 0, 0, 0.08));
}
.cover-title {
  margin: 0;
  font-size: 17px;
  font-weight: 600;
  color: #1e293b;
  white-space: nowrap;
}
.back-fab {
  position: absolute;
  left: 8px;
  top: 50%;
  transform: translateY(-50%);
  width: 32px;
  height: 32px;
  border: none;
  padding: 0;
  background: transparent;
  color: #1e293b;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.back-fab:hover {
  color: var(--spark-primary, #4f7cff);
}
.avatar-btn {
  position: absolute;
  right: 16px;
  bottom: -22px;
  border: none;
  padding: 0;
  background: transparent;
  cursor: pointer;
}
.avatar {
  width: 64px;
  height: 64px;
  border-radius: 8px;
  border: 3px solid #fff;
  object-fit: cover;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #e2e8f0;
  color: #475569;
  font-size: 26px;
}
.my-name {
  position: absolute;
  right: 92px;
  bottom: 20px; /* 名字底部高于背景图底部（不再超出背景） */
  color: #fff;
  font-weight: 600;
  font-size: 18px;
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
  /* 窄窗（320 最小窗）长昵称防溢出：左侧留白 12px，超出省略 */
  max-width: calc(100% - 104px);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.compose-fab {
  position: absolute;
  top: 50%;
  right: 16px;
  transform: translateY(-50%);
  width: 28px;
  height: 28px;
  border: none;
  padding: 0;
  background: transparent;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #1e293b;
  cursor: pointer;
  z-index: 5;
}
.compose-fab:hover {
  color: var(--spark-primary, #4f7cff);
}
.timeline {
  /* 滚动内容的一部分（在 .moments-scroll 内随封面一起滚动） */
  max-width: 640px;
  width: 100%;
  margin: 0 auto;
  padding: 24px 12px 40px;
  display: grid;
  gap: 12px;
  align-content: start;
}
.offline-bar {
  background: var(--spark-warning-bg, #fef3c7);
  color: var(--spark-warning, #b45309);
  padding: 8px 12px;
  border-radius: 6px;
  font-size: 12px;
}
.skeleton {
  display: grid;
  gap: 12px;
}
.skeleton-item {
  display: flex;
  gap: 12px;
  background: #fff;
  padding: 12px;
  border-radius: 8px;
}
.sk-avatar {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  background: #e2e8f0;
}
.sk-lines { flex: 1; display: grid; gap: 8px; }
.sk-line { height: 12px; border-radius: 4px; background: #e2e8f0; }
.sk-line.short { width: 60%; }
.end-hint {
  text-align: center;
  color: var(--spark-text-3, #94a3b8);
  font-size: 12px;
  padding: 12px 0;
}
</style>

<style>
/* 全局 reset（非 scoped）：iframe 宿主文档禁止整页滚动。
   宿主 srcdoc 只给空 #app（无预设样式），html/body/#app 高度链由插件自给
   （同 spark-minichat 先例）。锁定后整页不滚，只有内部各滚动区独立滚动。 */
html,
body,
#app {
  margin: 0;
  height: 100%;
  overflow: hidden;
}
</style>
