<!-- 聊天区（设计 §3）：头部 + 消息流（时间分隔/头像合并/系统灰条）+ 输入区 + 消息右键操作（§5.2） -->
<template>
  <section v-if="conversation" class="chat-view">
    <ChatHeader
      :space-key="spaceKey"
      :conversation="conversation"
      @back="$emit('back')"
      @removed="$emit('removed', $event)"
      @show-profile="openProfileCard(conversation.peerId)"
    />

    <div ref="scrollRef" class="msg-scroll">
      <!-- 应用会话（服务号模型 §20）：应用消息流（卡片富渲染/原生摘要降级），无人际气泡 -->
      <template v-if="isAppConversation">
        <template v-for="item in appRenderItems" :key="item.key">
          <div v-if="item.kind === 'time'" class="msg-time">{{ formatDividerTime(item.ts) }}</div>
          <AppMessageView
            v-else
            :message="item.msg"
            :space-key="spaceKey"
            :space="pluginSpaceContext"
            @open-market="openAppMarket"
          />
        </template>
        <el-empty
          v-if="!appRenderItems.length"
          :image-size="80"
          description="暂无应用消息"
          class="chat-empty"
        />
      </template>

      <template v-else>
        <template v-for="item in renderItems" :key="item.key">
          <div v-if="item.kind === 'time'" class="msg-time">{{ formatDividerTime(item.ts) }}</div>
          <div v-else-if="item.msg.recalled" class="msg-system">
            {{ item.msg.senderId === 'me' ? '你撤回了一条消息' : `「${recallName(item.msg)}」撤回了一条消息` }}
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
            @avatar-click="openProfileCard"
            @org-invite-click="openOrgInvite"
          />
        </template>
        <el-empty
          v-if="!renderItems.length"
          :image-size="80"
          description="暂无消息记录，开始聊天吧"
          class="chat-empty"
        />
      </template>
    </div>

    <!-- 仅本地模式提示条（真实 P2P 状态；仅人际会话有意义）：可关闭，切换会话后重置 -->
    <div v-if="conversation.kind === 'direct' && isLocalOnly && !localOnlyHintDismissed" class="local-only-bar">
      <el-icon :size="14"><WarningFilled /></el-icon>
      <span class="local-only-bar-text">对方离线，消息将在其上线后自动送达</span>
      <el-icon class="local-only-bar-close" :size="14" @click="localOnlyHintDismissed = true"><Close /></el-icon>
    </div>

    <!-- 已删除联系人提示条 -->
    <div v-if="contactDeleted" class="contact-deleted-bar">
      <el-icon :size="14"><WarningFilled /></el-icon>
      <span>对方已从你的通讯录中删除，仅可查看历史消息</span>
    </div>

    <MessageInput
      v-model="inputText"
      :quote="quote"
      :disabled="conversation.kind !== 'direct' || contactDeleted"
      :disabled-hint="contactDeleted ? '对方已删除，无法发送消息' : conversation.kind === 'app' ? '应用会话不支持回复' : '系统通知会话不支持回复'"
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

    <!-- 联系人资料卡抽屉：聊天气泡头像 / 聊天头点击弹出（复用通讯录 ContactPanel） -->
    <ContactCardDrawer v-model="profileCardVisible" :root-id="profileCardRootId" :space-key="spaceKey" />

    <!-- 组织邀请确认抽屉：系统会话邀请卡片（spark-org-invite:// 链接）点击弹出 -->
    <OrgInviteDrawer
      v-model="orgInviteVisible"
      :invite-id="orgInvite.inviteId"
      :org-id="orgInvite.orgId"
      :fallback-title="orgInvite.title"
      :fallback-description="orgInvite.description"
    />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { Close, WarningFilled } from '@element-plus/icons-vue';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import ChatHeader from './ChatHeader.vue';
import MessageBubble from './MessageBubble.vue';
import MessageInput from './MessageInput.vue';
import AppMessageView from './AppMessageView.vue';
import ContactCardDrawer from '../contacts/ContactCardDrawer.vue';
import OrgInviteDrawer from '../org/OrgInviteDrawer.vue';
import { useNetworkStatus } from '../../stores/network-status';
import { personDisplayName } from '../../stores/avatar-sources';
import { friendOf } from '../../mock/contacts';
import type { AppMessageDto } from '../../api/types';
import {
  closeConversation,
  deleteMessage,
  getAppMessages,
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
} from '../../stores/messages';

type RenderItem =
  | { kind: 'time'; ts: number; key: string }
  | { kind: 'msg'; msg: ChatMessage; showAvatar: boolean; key: string };

/** 应用会话渲染项：时间分隔条 / 应用消息（无头像合并语义） */
type AppRenderItem = { kind: 'time'; ts: number; key: string } | { kind: 'msg'; msg: AppMessageDto; key: string };

const TIME_GAP = 5 * 60_000;
const RECALL_WINDOW = 2 * 60_000;

export default defineComponent({
  name: 'ChatView',
  components: { ChatHeader, MessageBubble, MessageInput, AppMessageView, ContactCardDrawer, OrgInviteDrawer, Close, WarningFilled },
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

    // 联系人是否已被删除：direct 会话且 peerId 已不在通讯录（friendOf 返回 undefined）。
    // 已删除联系人的会话只用于查看历史消息，禁止再发送
    const contactDeleted = computed(() => {
      const conv = conversation.value;
      if (!conv || conv.kind !== 'direct') return false;
      const peerId = conv.peerId;
      if (!peerId) return false;
      return friendOf(props.spaceKey, peerId) === undefined;
    });

    // 应用会话：消息走 appMessages 缓存（appList 水合），渲染分支在模板
    const isAppConversation = computed(() => conversation.value?.kind === 'app');
    const appMessages = computed(() =>
      isAppConversation.value ? getAppMessages(props.spaceKey, props.conversationId) : []
    );
    /** 卡片 iframe 的运行上下文（spaceKey 反解；与 PluginIframeHost 的 space prop 同形） */
    const pluginSpaceContext = computed<PluginSpaceContext>(() =>
      props.spaceKey === 'personal'
        ? { type: 'personal', id: 'personal' }
        : { type: 'org', id: props.spaceKey.slice('org:'.length) }
    );

    /** 应用消息渲染项：仅时间分隔（间隔 > 5 分钟），无头像合并 */
    const appRenderItems = computed<AppRenderItem[]>(() => {
      const items: AppRenderItem[] = [];
      let prev: AppMessageDto | null = null;
      for (const msg of appMessages.value) {
        if (!prev || msg.createdAt - prev.createdAt > TIME_GAP) {
          items.push({ kind: 'time', ts: msg.createdAt, key: `t-${msg.id}` });
        }
        items.push({ kind: 'msg', msg, key: msg.id });
        prev = msg;
      }
      return items;
    });

    /** 「安装插件查看完整内容」：跳应用市场详情（App.vue 监听 spark:open-app 切页） */
    function openAppMarket(pluginId: string) {
      window.dispatchEvent(new CustomEvent('spark:open-app', { detail: { id: pluginId } }));
    }

    // 撤回提示：统一展示名入口（备注>昵称），消息上的 senderName 快照仅作兜底（快照语义不动）
    const recallName = (msg: ChatMessage): string => personDisplayName(props.spaceKey, msg.senderId, msg.senderName);

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

    // 新消息到达：若会话正打开则保持已读，并滚动到底部（应用会话同理）
    watch(
      () => [messages.value.length, appMessages.value.length],
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

    // ---- 联系人资料卡抽屉：气泡头像（senderId 即 rootId，MessageBubble 已换算 'me'）/ 聊天头点击 ----
    const profileCardVisible = ref(false);
    const profileCardRootId = ref('');

    function openProfileCard(rootId: string) {
      if (!rootId) {
        return;
      }
      profileCardRootId.value = rootId;
      profileCardVisible.value = true;
    }

    // ---- 组织邀请抽屉：系统会话邀请卡片点击（MessageBubble 已解析 inviteId/orgId） ----
    const orgInviteVisible = ref(false);
    const orgInvite = ref({ inviteId: '', orgId: '', title: '', description: '' });

    function openOrgInvite(payload: { inviteId: string; orgId: string; title: string; description: string }) {
      if (!payload.inviteId) {
        return;
      }
      orgInvite.value = payload;
      orgInviteVisible.value = true;
    }

    return {
      conversation,
      renderItems,
      contactDeleted,
      isAppConversation,
      appRenderItems,
      pluginSpaceContext,
      openAppMarket,
      inputText,
      quote,
      scrollRef,
      isLocalOnly,
      localOnlyHintDismissed,
      recallName,
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
      profileCardVisible,
      profileCardRootId,
      openProfileCard,
      orgInviteVisible,
      orgInvite,
      openOrgInvite,
      formatDividerTime
    };
  }
});
</script>
