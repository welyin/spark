<!-- 聊天区（设计 §3）：头部 + 消息流（时间分隔/头像合并/系统灰条）+ 输入区 + 消息右键操作（§5.2） -->
<template>
  <section v-if="conversation" class="chat-view">
    <ChatHeader
      :space-key="spaceKey"
      :conversation="conversation"
      @back="$emit('back')"
      @removed="$emit('removed', $event)"
    />

    <div ref="scrollRef" class="msg-scroll">
      <template v-for="item in renderItems" :key="item.key">
        <div v-if="item.kind === 'time'" class="msg-time">{{ formatDividerTime(item.ts) }}</div>
        <div v-else-if="item.msg.recalled" class="msg-system">
          {{ item.msg.senderId === 'me' ? '你撤回了一条消息' : `「${item.msg.senderName}」撤回了一条消息` }}
        </div>
        <div v-else-if="item.msg.type === 'system'" class="msg-system">{{ item.msg.content }}</div>
        <MessageBubble
          v-else
          :message="item.msg"
          :is-mine="item.msg.senderId === 'me'"
          :show-avatar="item.showAvatar"
          :space-key="spaceKey"
          @menu="openMsgMenu"
          @resend="onResend"
        />
      </template>
      <el-empty
        v-if="!renderItems.length"
        :image-size="80"
        description="暂无消息记录，开始聊天吧"
        class="chat-empty"
      />
    </div>

    <!-- 仅本地模式提示条（真实 P2P 状态）：可关闭，切换会话后重置 -->
    <div v-if="isLocalOnly && !localOnlyHintDismissed" class="local-only-bar">
      <el-icon :size="14"><WarningFilled /></el-icon>
      <span class="local-only-bar-text">对方离线，消息将在其上线后自动送达</span>
      <el-icon class="local-only-bar-close" :size="14" @click="localOnlyHintDismissed = true"><Close /></el-icon>
    </div>

    <MessageInput
      v-model="inputText"
      :quote="quote"
      :disabled="conversation.kind === 'system'"
      @send="onSend"
      @cancel-quote="quote = null"
    />

    <teleport to="body">
      <div v-if="msgMenu.visible" class="ctx-mask" @click="closeMsgMenu" @contextmenu.prevent="closeMsgMenu">
        <ul class="ctx-menu" :style="{ left: `${msgMenu.x}px`, top: `${msgMenu.y}px` }">
          <li v-if="canCopy" @click="onCopy">复制</li>
          <li @click="onQuote">引用回复</li>
          <li v-if="canRecall" @click="onRecall">撤回</li>
          <li class="danger" @click="onDeleteMsg">删除</li>
        </ul>
      </div>
    </teleport>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { Close, WarningFilled } from '@element-plus/icons-vue';
import ChatHeader from './ChatHeader.vue';
import MessageBubble from './MessageBubble.vue';
import MessageInput from './MessageInput.vue';
import { useNetworkStatus } from '../../stores/network-status';
import {
  closeConversation,
  deleteMessage,
  getConversation,
  getMessages,
  markRead,
  openConversation,
  previewText,
  recallMessage,
  resendMessage,
  sendText,
  setDraft,
  formatDividerTime,
  type ChatMessage,
  type QuoteRef,
  type SpaceKey
} from '../../mock/messages';

type RenderItem =
  | { kind: 'time'; ts: number; key: string }
  | { kind: 'msg'; msg: ChatMessage; showAvatar: boolean; key: string };

const TIME_GAP = 5 * 60_000;
const RECALL_WINDOW = 2 * 60_000;

export default defineComponent({
  name: 'ChatView',
  components: { ChatHeader, MessageBubble, MessageInput, Close, WarningFilled },
  props: {
    spaceKey: { type: String as () => SpaceKey, required: true },
    conversationId: { type: String, required: true }
  },
  emits: ['back', 'removed'],
  setup(props) {
    const scrollRef = ref<HTMLElement>();
    const inputText = ref('');
    const quote = ref<QuoteRef | null>(null);
    // 仅本地提示条：本次会话内可关闭，切换会话后重新出现
    const { isLocalOnly } = useNetworkStatus();
    const localOnlyHintDismissed = ref(false);

    const conversation = computed(() => getConversation(props.spaceKey, props.conversationId));
    const messages = computed(() => getMessages(props.spaceKey, props.conversationId));

    // 时间分隔（间隔 > 5 分钟）与同一分钟内连续消息头像合并（§3.2）
    const renderItems = computed<RenderItem[]>(() => {
      const items: RenderItem[] = [];
      let prev: ChatMessage | null = null;
      for (const msg of messages.value) {
        if (!prev || msg.createdAt - prev.createdAt > TIME_GAP) {
          items.push({ kind: 'time', ts: msg.createdAt, key: `t-${msg.id}` });
        }
        const sameMinute =
          !!prev &&
          msg.senderId === prev.senderId &&
          msg.createdAt - prev.createdAt < 60_000 &&
          msg.createdAt - prev.createdAt <= TIME_GAP;
        items.push({ kind: 'msg', msg, showAvatar: !sameMinute, key: msg.id });
        prev = msg;
      }
      return items;
    });

    function scrollToBottom() {
      void nextTick(() => {
        const el = scrollRef.value;
        if (el) el.scrollTop = el.scrollHeight;
      });
    }

    // 切换会话：保存旧草稿、载入新草稿、记录当前会话并清零未读
    watch(
      () => [props.spaceKey, props.conversationId],
      ([key, id], old) => {
        if (old && old[1]) setDraft(old[0] as SpaceKey, old[1] as string, inputText.value);
        inputText.value = getConversation(key as SpaceKey, id as string)?.draft ?? '';
        quote.value = null;
        localOnlyHintDismissed.value = false;
        openConversation(key as SpaceKey, id as string);
        scrollToBottom();
      },
      { immediate: true }
    );

    // 新消息到达：若会话正打开则保持已读，并滚动到底部
    watch(
      () => messages.value.length,
      () => {
        markRead(props.spaceKey, props.conversationId);
        scrollToBottom();
      }
    );

    onBeforeUnmount(() => {
      setDraft(props.spaceKey, props.conversationId, inputText.value);
      closeConversation(props.spaceKey);
    });

    function onSend(text: string) {
      if (!sendText(props.spaceKey, props.conversationId, text, quote.value ?? undefined)) return;
      inputText.value = '';
      quote.value = null;
      scrollToBottom();
    }

    // ---- 消息右键菜单（§5.2）：复制 / 引用回复 / 撤回（2 分钟内）/ 删除（本地） ----
    const msgMenu = ref<{ visible: boolean; x: number; y: number; msg: ChatMessage | null }>({
      visible: false,
      x: 0,
      y: 0,
      msg: null
    });

    const canCopy = computed(() => msgMenu.value.msg?.type === 'text' || msgMenu.value.msg?.type === 'link');
    const canRecall = computed(() => {
      const msg = msgMenu.value.msg;
      return !!msg && msg.senderId === 'me' && !msg.recalled && Date.now() - msg.createdAt < RECALL_WINDOW;
    });

    function openMsgMenu(payload: { event: MouseEvent; message: ChatMessage }) {
      msgMenu.value = { visible: true, x: payload.event.clientX, y: payload.event.clientY, msg: payload.message };
    }

    function closeMsgMenu() {
      msgMenu.value = { ...msgMenu.value, visible: false, msg: null };
    }

    async function onCopy() {
      const msg = msgMenu.value.msg;
      closeMsgMenu();
      if (!msg) return;
      try {
        await navigator.clipboard.writeText(msg.type === 'link' ? (msg.link?.url ?? msg.content) : msg.content);
        ElMessage.success('已复制');
      } catch {
        ElMessage.error('复制失败');
      }
    }

    function onQuote() {
      const msg = msgMenu.value.msg;
      closeMsgMenu();
      if (!msg) return;
      quote.value = { messageId: msg.id, senderName: msg.senderName, preview: previewText(msg) };
    }

    function onRecall() {
      const msg = msgMenu.value.msg;
      closeMsgMenu();
      if (!msg) return;
      if (!recallMessage(props.spaceKey, props.conversationId, msg.id)) {
        ElMessage.warning('发送超过 2 分钟，无法撤回');
      }
    }

    function onDeleteMsg() {
      const msg = msgMenu.value.msg;
      closeMsgMenu();
      if (msg) deleteMessage(props.spaceKey, props.conversationId, msg.id);
    }

    function onResend(msg: ChatMessage) {
      resendMessage(props.spaceKey, props.conversationId, msg.id);
    }

    return {
      conversation,
      renderItems,
      inputText,
      quote,
      scrollRef,
      isLocalOnly,
      localOnlyHintDismissed,
      msgMenu,
      canCopy,
      canRecall,
      openMsgMenu,
      closeMsgMenu,
      onCopy,
      onQuote,
      onRecall,
      onDeleteMsg,
      onSend,
      onResend,
      formatDividerTime
    };
  }
});
</script>
