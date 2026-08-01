<!-- 消息页：会话列表 + 聊天区（设计 ui-messages §2/§3），数据全部来自 mock store。
     移动端（波次 2，窄屏 ≤768px）：整页 + 导航栈——栈1 会话列表，点开会话 push 聊天页（栈2）整页，
     返回用聊天头已有的 ‹ 按钮（pop）；桌面端渲染逻辑不变 -->
<template>
  <section class="messages-page">
    <!-- 移动端仅栈深 1（未选中会话）时渲染列表；桌面端常驻 -->
    <ConversationList
      v-if="!isMobileLayout || !activeId"
      :space-key="spaceKey"
      :space-type="spaceType"
      :active-id="activeId"
      @select="onSelectConversation"
      @removed="onRemoved"
    />
    <ChatView
      v-if="activeId"
      :key="`${spaceKey}:${activeId}`"
      :space-key="spaceKey"
      :conversation-id="activeId"
      @back="onChatBack"
      @removed="onRemoved"
    />
    <!-- 空态占位仅桌面端：移动端栈深 1 时整页为会话列表 -->
    <div v-else-if="!isMobileLayout" class="chat-placeholder">
      <el-empty :image-size="110" description="选择一个会话开始聊天">
        <div class="chat-placeholder-actions">
          <el-button type="primary" @click="goBrowseContacts">发起新会话</el-button>
          <el-button @click="goAddContact">{{ spaceType === 'org' ? '添加成员' : '添加朋友' }}</el-button>
        </div>
      </el-empty>
    </div>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import ConversationList from '../components/messages/ConversationList.vue';
import ChatView from '../components/messages/ChatView.vue';
import { currentSpace, currentSpaceType } from '../stores/current-space';
import { consumePendingChat, pendingChat } from '../stores/pending-chat';
import { isMobileLayout } from '../stores/ui-layout';
import { currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import { ensureDirectConversation, spaceKeyOf } from '../mock/messages';
import { CONTACT_INTENT_ADD, CONTACT_INTENT_BROWSE, openContacts } from '../components/contacts/open-intents';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'messages';

export default defineComponent({
  name: 'MessagesPage',
  components: { ConversationList, ChatView },
  setup() {
    const spaceKey = computed(() => spaceKeyOf(currentSpace.value));
    const spaceType = computed(() => currentSpaceType.value);
    const activeId = ref('');

    // 切换空间后重选会话（两个空间的会话数据互相隔离）；移动端同步回栈底（会话不跨空间）
    watch(spaceKey, () => {
      activeId.value = '';
      resetStack(MOBILE_TAB);
    });

    // 移动端（波次 2）：栈顶帧变化（重进 tab 按栈恢复 / 返回 pop / 重按 tab 复位）时同步选中会话；
    // 桌面选中会话后拖窗进入窄屏时按当前选中补一帧聊天页，避免直接丢回列表
    watch(
      [() => currentPage(MOBILE_TAB), isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        if (frame.page === 'chat') {
          activeId.value = frame.params?.id ?? '';
        } else if (activeId.value) {
          pushPage(MOBILE_TAB, 'chat', { id: activeId.value });
        } else {
          activeId.value = '';
        }
      },
      { immediate: true }
    );

    /** 选中会话：桌面仅切换右栏；移动端同时压入聊天页栈帧（栈2 整页） */
    const onSelectConversation = (id: string) => {
      activeId.value = id;
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'chat', { id });
      }
    };

    /** 聊天头返回：桌面清空选中回占位；移动端 pop 回会话列表（栈1） */
    const onChatBack = () => {
      if (isMobileLayout.value) {
        popPage(MOBILE_TAB);
      }
      activeId.value = '';
    };

    // 消费「打开会话」请求（通讯录/应用市场跳转）：找到或创建 1:1 会话并选中
    const openPendingChat = () => {
      const peer = consumePendingChat();
      if (!peer) {
        return;
      }
      // 已有会话 id 直接选中（全局搜索跳转）；否则按 peerId 找到或创建 1:1 会话
      onSelectConversation(
        peer.conversationId ??
        ensureDirectConversation(spaceKey.value, peer.rootId, peer.name || `${peer.rootId.slice(0, 8)}…`)
      );
    };
    onMounted(openPendingChat);
    // 消息页已激活时又收到新请求
    watch(pendingChat, openPendingChat);

    function onRemoved(id: string) {
      if (activeId.value === id) activeId.value = '';
      // 移动端：被删会话的栈帧一并弹出，避免返回到已删除的空会话页
      if (isMobileLayout.value && currentPage(MOBILE_TAB).params?.id === id) {
        popPage(MOBILE_TAB);
      }
    }

    // 空状态引导（App.vue 监听 spark:open-contact 切到通讯录页，ContactsPage 消费意图）
    const goBrowseContacts = () => openContacts(CONTACT_INTENT_BROWSE);
    const goAddContact = () => openContacts(CONTACT_INTENT_ADD);

    return { spaceKey, spaceType, activeId, isMobileLayout, onSelectConversation, onChatBack, onRemoved, goBrowseContacts, goAddContact };
  }
});
</script>

<!-- 非 scoped：样式需作用于子组件（与 org.css 同模式），选择器统一以 .messages-page 前缀隔离；.ctx-* 为 teleport 到 body 的右键菜单 -->
<style src="../styles/pages/messages.css"></style>
