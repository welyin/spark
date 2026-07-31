<!-- 聊天头部（设计 §3.1）：返回、头像名称+连接状态标签、语音/视频通话（占位，下一期实现）、更多操作（§5.1） -->
<template>
  <header class="chat-header">
    <el-button text :icon="ArrowLeft" class="chat-back" @click="$emit('back')" />
    <!-- direct 会话：头像+昵称区域整块可点，弹出对方资料卡抽屉；system 会话不可点 -->
    <div
      class="chat-profile"
      :class="{ 'chat-profile-clickable': profileClickable }"
      :role="profileClickable ? 'button' : undefined"
      :title="profileClickable ? '查看资料' : undefined"
      @click="onProfileClick"
    >
      <UserAvatar :root-id="conversation.peerId" :nickname="peerName" :avatar="peerAvatar" :size="32" />
      <div class="chat-title">
        <span class="chat-name">{{ peerName }}</span>
        <span class="chat-sub">
          <template v-if="conversation.kind === 'system'">系统通知</template>
          <template v-else-if="conversation.kind === 'app'">
            应用消息
            <span v-if="isBlocked" class="chat-sub-extra">已屏蔽</span>
            <span v-if="conversation.muted" class="chat-sub-extra">消息免打扰</span>
          </template>
          <template v-else>
            <!-- 真实状态优先：仅本地时统一「仅本地·不可达」，否则用 mock 的 online 字段 -->
            <span class="chat-presence-tag" :class="presenceClass">
              <el-icon :size="12"><component :is="presenceIcon" /></el-icon>
              {{ presenceText }}
            </span>
            <span v-if="conversation.muted" class="chat-sub-extra">消息免打扰</span>
          </template>
        </span>
      </div>
    </div>
    <div class="chat-actions">
      <!-- TODO(mock): 音视频通话仅占位，点击提示下一期实现（设计 §3.1 注） -->
      <el-tooltip content="语音通话（下一期实现）" placement="bottom">
        <el-button text :icon="Phone" @click="onCallPlaceholder" />
      </el-tooltip>
      <el-tooltip content="视频通话（下一期实现）" placement="bottom">
        <el-button text :icon="VideoCamera" @click="onCallPlaceholder" />
      </el-tooltip>
      <el-dropdown trigger="click" @command="onCommand">
        <el-button text :icon="MoreFilled" />
        <template #dropdown>
          <el-dropdown-menu>
            <el-dropdown-item command="pin" :icon="Top">
              {{ conversation.pinnedAt > 0 ? '取消置顶' : '置顶聊天' }}
            </el-dropdown-item>
            <el-dropdown-item command="mute" :icon="MuteNotification">
              {{ conversation.muted ? '取消免打扰' : '消息免打扰' }}
            </el-dropdown-item>
            <!-- 应用会话：屏蔽为本地持久化状态（抑制未读角标与聚合） -->
            <el-dropdown-item v-if="conversation.kind === 'app'" command="block" :icon="Remove">
              {{ isBlocked ? '取消屏蔽' : '屏蔽应用消息' }}
            </el-dropdown-item>
            <!-- 应用消息内核无「清空」接口（仅删除会话），应用会话不展示清空项 -->
            <el-dropdown-item v-if="conversation.kind !== 'app'" command="clear" :icon="Brush" divided>
              <span class="ctx-danger">清空聊天记录</span>
            </el-dropdown-item>
            <el-dropdown-item command="delete" :icon="Delete">
              <span class="ctx-danger">删除会话</span>
            </el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>
    </div>
  </header>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import {
  ArrowLeft,
  Brush,
  CircleCheckFilled,
  Delete,
  MoreFilled,
  MuteNotification,
  Phone,
  Remove,
  RemoveFilled,
  Top,
  VideoCamera,
  WarningFilled
} from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { isLocalOnly } from '../../stores/network-status';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import { appConversationName, isAppConversationBlocked, toggleAppConversationBlocked } from '../../stores/app-conversations';
import {
  clearMessages,
  deleteConversation,
  toggleMute,
  togglePin,
  type Conversation,
  type SpaceKey
} from '../../mock/messages';

export default defineComponent({
  name: 'ChatHeader',
  components: { UserAvatar },
  props: {
    spaceKey: { type: String as () => SpaceKey, required: true },
    conversation: { type: Object as PropType<Conversation>, required: true }
  },
  emits: ['back', 'removed', 'show-profile'],
  setup(props, { emit }) {
    // 仅 direct 会话头部可点开资料卡（peerId 即对方 rootId）
    const profileClickable = computed(() => props.conversation.kind === 'direct');
    function onProfileClick() {
      if (profileClickable.value) {
        emit('show-profile');
      }
    }
    // 连接状态：仅本地（真实 P2P 状态）优先于 mock online 字段；免打扰后缀拆为独立弱提示
    const presenceClass = computed(() => {
      if (isLocalOnly.value) return 'is-local';
      return props.conversation.online ? 'is-online' : 'is-offline';
    });
    const presenceIcon = computed(() => {
      if (isLocalOnly.value) return WarningFilled;
      return props.conversation.online ? CircleCheckFilled : RemoveFilled;
    });
    const presenceText = computed(() =>
      isLocalOnly.value ? '仅本地·不可达' : props.conversation.online ? '在线' : '离线'
    );

    // direct 会话的 peerId 即对方 rootId：统一头像入口（朋友记录优先），无则走自动头像
    const peerAvatar = computed(() =>
      props.conversation.kind === 'direct' ? personAvatarSource(props.spaceKey, props.conversation.peerId).image : ''
    );

    // 会话标题：direct 会话走统一展示名入口（备注>昵称>会话原标题），改备注全网同步；
    // app 会话走插件清单名称（缺省 pluginId）
    const peerName = computed(() => {
      if (props.conversation.kind === 'app') {
        return appConversationName(props.conversation.peerId, props.conversation.title);
      }
      return props.conversation.kind === 'direct'
        ? personDisplayName(props.spaceKey, props.conversation.peerId, props.conversation.title)
        : props.conversation.title;
    });

    /** 应用会话屏蔽态（本地持久化；抑制未读角标与聚合） */
    const isBlocked = computed(
      () => props.conversation.kind === 'app' && isAppConversationBlocked(props.spaceKey, props.conversation.peerId)
    );

    // TODO(mock): 音视频通话下一期实现，点击仅提示
    function onCallPlaceholder() {
      ElMessage.info('通话功能将在下一期实现');
    }

    async function onCommand(command: string) {
      const conv = props.conversation;
      if (command === 'pin') {
        togglePin(props.spaceKey, conv.id);
      } else if (command === 'mute') {
        toggleMute(props.spaceKey, conv.id);
      } else if (command === 'block') {
        // 应用会话屏蔽/取消屏蔽（本地持久化，stores/app-conversations）
        if (conv.kind === 'app') {
          toggleAppConversationBlocked(props.spaceKey, conv.peerId);
        }
      } else if (command === 'clear') {
        try {
          await ElMessageBox.confirm('仅删除本地消息记录，不影响对方设备。', `清空与「${peerName.value}」的聊天记录？`, {
            confirmButtonText: '清空',
            cancelButtonText: '取消',
            type: 'warning'
          });
          clearMessages(props.spaceKey, conv.id);
        } catch {
          // 用户取消
        }
      } else if (command === 'delete') {
        try {
          // 应用会话删除连带清空该插件在本空间的全部应用消息（§20.1），文案如实说明
          const hint = conv.kind === 'app' ? '将同时删除该应用在本空间的全部消息记录。' : '仅删除会话列表入口。';
          await ElMessageBox.confirm(hint, `删除与「${peerName.value}」的会话？`, {
            confirmButtonText: '删除',
            cancelButtonText: '取消',
            type: 'warning'
          });
          deleteConversation(props.spaceKey, conv.id);
          emit('removed', conv.id);
        } catch {
          // 用户取消
        }
      }
    }

    return {
      onCommand,
      onCallPlaceholder,
      profileClickable,
      onProfileClick,
      presenceClass,
      presenceIcon,
      presenceText,
      peerAvatar,
      peerName,
      isBlocked,
      ArrowLeft,
      Phone,
      VideoCamera,
      MoreFilled,
      Top,
      MuteNotification,
      Remove,
      Brush,
      Delete
    };
  }
});
</script>

<style scoped>
/* 免打扰后缀：状态标签旁的弱提示 */
.chat-sub-extra {
  color: var(--spark-text-3);
}
</style>
