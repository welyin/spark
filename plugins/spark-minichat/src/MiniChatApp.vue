<!-- 最小聊天插件根视图（spark-minichat）：左会话列表 + 右消息区/输入框，
     刻意无样式框架（一个 <style> 块内原生 CSS）——验收样例聚焦「最小面」：
     全部数据经 sdk.messages 四调用，与默认内置聊天插件读写同一消息数据。 -->
<template>
  <section class="minichat">
    <!-- 侧栏：宽窗常驻两栏；窄窗（≤560px，ui-layout.isNarrowLayout）折叠为
         覆盖抽屉，头部 × / 遮罩点击 / 选中会话均可收起 -->
    <aside
      class="minichat-convs"
      :class="{ 'minichat-convs--overlay': isNarrowLayout, 'minichat-convs--open': sidebarOpen }"
    >
      <div class="minichat-side-head">
        <h2 class="minichat-title">最小聊天（{{ spaceLabel }}）</h2>
        <button
          v-if="isNarrowLayout"
          type="button"
          class="minichat-side-close"
          title="收起会话列表"
          aria-label="收起会话列表"
          @click="closeSidebar"
        >&times;</button>
      </div>
      <button
        v-for="conv in conversations"
        :key="conv.id"
        type="button"
        class="minichat-conv"
        :class="{ active: conv.id === activeConvId }"
        @click="onOpenConversation(conv.id)"
      >
        <b>{{ conv.title }}</b>
        <span v-if="conv.unreadCount > 0" class="minichat-unread">{{ conv.unreadCount }}</span>
      </button>
      <p v-if="conversations.length === 0" class="minichat-empty">暂无会话</p>
    </aside>
    <!-- 窄窗抽屉遮罩：点击空白处收起侧栏 -->
    <div
      v-if="isNarrowLayout && sidebarOpen"
      class="minichat-backdrop"
      @click="closeSidebar"
    ></div>
    <main class="minichat-main">
      <!-- 窄窗：会话区全宽，☰ 为打开侧栏入口 -->
      <div v-if="isNarrowLayout" class="minichat-main-head">
        <button
          type="button"
          class="minichat-side-toggle"
          title="会话列表"
          aria-label="打开会话列表"
          @click="openSidebar"
        >☰</button>
      </div>
      <template v-if="activeConvId">
        <ul class="minichat-msgs">
          <li v-for="msg in activeMessages" :key="msg.id" :class="{ mine: msg.senderId === 'me' }">
            <b>{{ msg.senderName }}：</b>
            <span v-if="msg.recalled">[消息已撤回]</span>
            <span v-else>{{ msg.content }}</span>
          </li>
        </ul>
        <form class="minichat-input" @submit.prevent="onSend">
          <input v-model="draft" placeholder="输入消息，回车发送" />
          <button type="submit">发送</button>
        </form>
      </template>
      <p v-else class="minichat-empty">选择一个会话</p>
    </main>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import {
  activeConvId,
  activeMessages,
  conversations,
  minichatSpace,
  openConversation,
  refreshConversations,
  sendText,
  subscribeNewMessages
} from './sdk-access';
import { isNarrowLayout } from './ui-layout';

export default defineComponent({
  name: 'MiniChatApp',
  setup() {
    const draft = ref('');
    const spaceLabel = computed(() => (minichatSpace().type === 'org' ? '组织空间' : '个人空间'));
    const onSend = () => {
      void sendText(draft.value);
      draft.value = '';
    };
    /** 窄窗抽屉开态（宽窗下无意义：侧栏常驻，--overlay 类不生效） */
    const sidebarOpen = ref(false);
    const openSidebar = () => {
      sidebarOpen.value = true;
    };
    const closeSidebar = () => {
      sidebarOpen.value = false;
    };
    // 宽窄模式切换时复位抽屉：切宽窗抽屉状态无意义，切窄窗默认收起
    watch(isNarrowLayout, () => {
      sidebarOpen.value = false;
    });
    // 窄窗：选中会话后收起抽屉，会话区全宽展示
    const onOpenConversation = (convId: string) => {
      void openConversation(convId);
      closeSidebar();
    };
    onMounted(() => {
      void refreshConversations();
      void subscribeNewMessages();
    });
    return {
      conversations,
      activeConvId,
      activeMessages,
      draft,
      spaceLabel,
      isNarrowLayout,
      sidebarOpen,
      openSidebar,
      closeSidebar,
      onOpenConversation,
      onSend
    };
  }
});
</script>

<style>
/* 锁定文档根容器高度：根容器 height:100% 的百分比级联由宿主 srcdoc 的
   html/body/#app 链给定（同 ai-chat 先例；100vh 在 iframe 内等同窗口高，
   统一为 100% 口径） */
html, body, #app { height: 100%; margin: 0; }
.minichat { display: flex; height: 100%; position: relative; font-family: sans-serif; overflow: hidden; }
.minichat-convs { width: 240px; border-right: 1px solid #ddd; overflow-y: auto; }
.minichat-side-head { display: flex; align-items: center; justify-content: space-between; padding: 12px; }
.minichat-title { font-size: 14px; margin: 0; }
.minichat-conv { display: flex; justify-content: space-between; width: 100%; padding: 10px 12px; border: 0; background: none; cursor: pointer; text-align: left; }
.minichat-conv.active { background: #ecf5ff; }
.minichat-unread { background: #f56c6c; color: #fff; border-radius: 8px; padding: 0 6px; font-size: 12px; }
.minichat-main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
.minichat-msgs { flex: 1; overflow-y: auto; list-style: none; margin: 0; padding: 12px; }
.minichat-msgs li { margin-bottom: 8px; }
.minichat-msgs li.mine { text-align: right; }
.minichat-input { display: flex; gap: 8px; padding: 12px; border-top: 1px solid #ddd; }
.minichat-input input { flex: 1; padding: 6px 8px; min-width: 0; }
.minichat-empty { padding: 12px; color: #999; }

/* 窄窗布局（≤560px，ui-layout.isNarrowLayout）：侧栏折叠为覆盖抽屉，
   会话区全宽；☰ 打开，遮罩点击 / 头部 × / 选中会话均可收起 */
.minichat-main-head { display: flex; padding: 6px 8px 0; }
.minichat-side-toggle { width: 28px; height: 28px; border: 1px solid #ddd; border-radius: 6px; background: #fff; cursor: pointer; font-size: 14px; line-height: 26px; }
.minichat-side-close { width: 24px; height: 24px; border: 0; background: none; color: #999; cursor: pointer; font-size: 16px; line-height: 24px; }
.minichat-convs--overlay { position: absolute; top: 0; left: 0; bottom: 0; z-index: 30; max-width: 85vw; background: #fff; box-shadow: 8px 0 24px rgba(0, 0, 0, 0.12); transform: translateX(-105%); transition: transform 0.2s ease; }
.minichat-convs--overlay.minichat-convs--open { transform: translateX(0); }
.minichat-backdrop { position: absolute; inset: 0; z-index: 20; background: rgba(0, 0, 0, 0.25); }
</style>
