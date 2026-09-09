<!-- 聊天应用根视图（spark-chat，communication §4.2 迁移的 MessagesPage 组合）：
     会话列表 + 聊天区。space 由桥绑定（sdk-host.boundSpaceKey）；
     移动端按视口宽度在列表/聊天之间切换（插件内自包含布局，ui-layout）。 -->
<template>
  <section class="messages-page">
    <!-- 移动端：未选会话整页列表，选中后整页聊天（返回经聊天头 ‹） -->
    <template v-if="isMobileLayout">
      <ConversationList
        v-if="!activeId"
        :space-key="spaceKey"
        :space-type="spaceType"
        :active-id="activeId"
        @select="onSelectConversation"
        @removed="onRemoved"
      />
      <ChatView
        v-else
        :key="`${spaceKey}:${activeId}`"
        :space-key="spaceKey"
        :conversation-id="activeId"
        @back="onChatBack"
        @removed="onRemoved"
      />
    </template>

    <!-- 桌面端：列表常驻 + 聊天区/占位 -->
    <template v-else>
      <ConversationList
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
      <div v-else class="chat-placeholder">
        <el-empty :image-size="110" description="选择一个会话开始聊天" />
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import ConversationList from './components/ConversationList.vue';
import ChatView from './components/ChatView.vue';
import { boundSpaceKey, pluginSpace } from './sdk-host';
import { isMobileLayout } from './ui-layout';

export default defineComponent({
  name: 'ChatApp',
  components: { ConversationList, ChatView },
  setup() {
    const spaceKey = computed(() => boundSpaceKey());
    const spaceType = computed(() => pluginSpace().type);
    const activeId = ref('');

    function onSelectConversation(convId: string) {
      activeId.value = convId;
    }
    function onChatBack() {
      activeId.value = '';
    }
    function onRemoved(convId: string) {
      if (activeId.value === convId) activeId.value = '';
    }

    return { spaceKey, spaceType, activeId, isMobileLayout, onSelectConversation, onChatBack, onRemoved };
  }
});
</script>

<style>
/* 迁移自壳层 messages 页样式（.messages-page 作用域）；tokens 变量自包含 */
@import './styles/tokens.css';
@import './styles/pages/messages.css';
</style>
