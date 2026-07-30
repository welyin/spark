<!-- 聊天头部（设计 §3.1）：返回、头像名称+连接状态标签、语音/视频通话（占位，下一期实现）、更多操作（§5.1） -->
<template>
  <header class="chat-header">
    <el-button text :icon="ArrowLeft" class="chat-back" @click="$emit('back')" />
    <UserAvatar :root-id="conversation.peerId" :nickname="peerName" :avatar="peerAvatar" :size="32" />
    <div class="chat-title">
      <span class="chat-name">{{ peerName }}</span>
      <span class="chat-sub">
        <template v-if="conversation.kind === 'system'">系统通知</template>
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
            <el-dropdown-item command="clear" :icon="Brush" divided>
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
  RemoveFilled,
  Top,
  VideoCamera,
  WarningFilled
} from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { isLocalOnly } from '../../stores/network-status';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
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
  emits: ['back', 'removed'],
  setup(props, { emit }) {
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

    // 会话标题：direct 会话走统一展示名入口（备注>昵称>会话原标题），改备注全网同步
    const peerName = computed(() =>
      props.conversation.kind === 'direct'
        ? personDisplayName(props.spaceKey, props.conversation.peerId, props.conversation.title)
        : props.conversation.title
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
          await ElMessageBox.confirm('仅删除会话列表入口。', `删除与「${peerName.value}」的会话？`, {
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
      presenceClass,
      presenceIcon,
      presenceText,
      peerAvatar,
      peerName,
      ArrowLeft,
      Phone,
      VideoCamera,
      MoreFilled,
      Top,
      MuteNotification,
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
