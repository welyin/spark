<!-- 消息页：会话列表 + 聊天区（设计 ui-messages §2/§3），数据全部来自 mock store -->
<template>
  <section class="messages-page">
    <ConversationList
      :space-key="spaceKey"
      :space-type="spaceType"
      :active-id="activeId"
      @select="activeId = $event"
      @removed="onRemoved"
    />
    <ChatView
      v-if="activeId"
      :key="`${spaceKey}:${activeId}`"
      :space-key="spaceKey"
      :conversation-id="activeId"
      @back="activeId = ''"
      @removed="onRemoved"
    />
    <div v-else class="chat-placeholder">
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
import { ensureDirectConversation, spaceKeyOf } from '../mock/messages';
import { CONTACT_INTENT_ADD, CONTACT_INTENT_BROWSE, openContacts } from '../components/contacts/open-intents';

export default defineComponent({
  name: 'MessagesPage',
  components: { ConversationList, ChatView },
  setup() {
    const spaceKey = computed(() => spaceKeyOf(currentSpace.value));
    const spaceType = computed(() => currentSpaceType.value);
    const activeId = ref('');

    // 切换空间后重选会话（两个空间的会话数据互相隔离）
    watch(spaceKey, () => {
      activeId.value = '';
    });

    // 消费「打开会话」请求（通讯录/应用市场跳转）：找到或创建 1:1 会话并选中
    const openPendingChat = () => {
      const peer = consumePendingChat();
      if (!peer) {
        return;
      }
      // 已有会话 id 直接选中（全局搜索跳转）；否则按 peerId 找到或创建 1:1 会话
      activeId.value =
        peer.conversationId ??
        ensureDirectConversation(spaceKey.value, peer.rootId, peer.name || `${peer.rootId.slice(0, 8)}…`);
    };
    onMounted(openPendingChat);
    // 消息页已激活时又收到新请求
    watch(pendingChat, openPendingChat);

    function onRemoved(id: string) {
      if (activeId.value === id) activeId.value = '';
    }

    // 空状态引导（App.vue 监听 spark:open-contact 切到通讯录页，ContactsPage 消费意图）
    const goBrowseContacts = () => openContacts(CONTACT_INTENT_BROWSE);
    const goAddContact = () => openContacts(CONTACT_INTENT_ADD);

    return { spaceKey, spaceType, activeId, onRemoved, goBrowseContacts, goAddContact };
  }
});
</script>

<!-- 非 scoped：样式需作用于子组件（与 org.css 同模式），选择器统一以 .messages-page 前缀隔离；.ctx-* 为 teleport 到 body 的右键菜单 -->
<style src="../styles/pages/messages.css"></style>
