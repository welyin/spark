<!-- 消息气泡（设计 §3.2/§4）：文本/图片/文件/链接/语音卡片、引用块、发送状态（§3.3） -->
<template>
  <div class="msg-row" :class="{ mine: isMine }">
    <div class="msg-avatar">
      <!-- 自己的消息：头像/昵称/配色种子统一走 avatar-sources（与其它位置的个人头像一致，
           mock 的 senderId 'me' 会得到不同哈希色，不能直接用） -->
      <UserAvatar
        v-if="showAvatar"
        :root-id="avatarSeed"
        :nickname="avatarName"
        :avatar="avatarImage"
        :size="36"
      />
    </div>
    <div class="msg-body">
      <div class="msg-bubble" :class="`is-${message.type}`" @contextmenu.prevent="onMenu">
        <div v-if="message.quote" class="msg-quote">
          <span class="msg-quote-name">{{ message.quote.senderName }}：</span>
          <span>{{ message.quote.preview }}</span>
        </div>

        <template v-if="message.type === 'image'">
          <div class="msg-image">
            <el-icon :size="28"><Picture /></el-icon>
            <span>[图片]</span>
          </div>
        </template>

        <template v-else-if="message.type === 'file'">
          <div class="msg-file">
            <el-icon :size="30" class="msg-file-icon"><Document /></el-icon>
            <div class="msg-file-info">
              <span class="msg-file-name">{{ message.content }}</span>
              <span class="msg-file-size">{{ formatBytes(message.fileSize ?? 0) }}</span>
            </div>
          </div>
        </template>

        <template v-else-if="message.type === 'voice'">
          <div class="msg-voice">
            <el-icon :size="16"><Microphone /></el-icon>
            <span class="msg-voice-bar" />
            <span>{{ message.duration ?? 0 }}″</span>
          </div>
        </template>

        <template v-else>
          <span class="msg-text">{{ message.content }}</span>
        </template>

        <!-- 链接预览卡片（设计 §6）：标题 + 图标 + 描述 + 来源 -->
        <a
          v-if="message.link"
          class="link-card"
          :href="message.link.url"
          target="_blank"
          rel="noreferrer"
          @click.stop
        >
          <div class="link-card-head">
            <el-icon :size="16" class="link-card-icon"><Link /></el-icon>
            <span class="link-card-title">{{ message.link.title }}</span>
          </div>
          <p class="link-card-desc">{{ message.link.description }}</p>
          <p class="link-card-source">来自：{{ message.link.siteName }} / {{ message.link.domain }}</p>
        </a>
      </div>

      <!-- 消息状态（§3.3）：⌛ 发送中 / ✓ 已发送 / ✓✓ 已送达 / 已读 / ⚠ 失败 -->
      <span v-if="isMine && message.status" class="msg-status" :class="`st-${message.status}`">
        <template v-if="message.status === 'sending'">⌛</template>
        <template v-else-if="message.status === 'sent'">✓</template>
        <template v-else-if="message.status === 'delivered'">✓✓</template>
        <template v-else-if="message.status === 'read'">已读</template>
        <el-tooltip v-else content="发送失败，点击重发" placement="top">
          <button class="msg-retry" @click="$emit('resend', message)">⚠</button>
        </el-tooltip>
      </span>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { Document, Link, Microphone, Picture } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { formatBytes } from '../../utils/format';
import { personalAvatarSource } from '../../stores/avatar-sources';
import type { ChatMessage } from '../../mock/messages';

export default defineComponent({
  name: 'MessageBubble',
  components: { UserAvatar, Picture, Document, Microphone, Link },
  props: {
    message: { type: Object as PropType<ChatMessage>, required: true },
    isMine: { type: Boolean, default: false },
    /** 同一分钟内连续消息合并头像（§3.2） */
    showAvatar: { type: Boolean, default: true }
  },
  emits: ['menu', 'resend'],
  setup(props, { emit }) {
    // 自己的消息统一走 avatar-sources（未加载完成时回退消息自带字段）
    const avatarSeed = computed(() => {
      if (!props.isMine) {
        return props.message.senderId;
      }
      return personalAvatarSource().seed || props.message.senderId;
    });
    const avatarName = computed(() => {
      if (!props.isMine) {
        return props.message.senderName;
      }
      const source = personalAvatarSource();
      return source.seed ? source.name : props.message.senderName;
    });
    const avatarImage = computed(() => (props.isMine ? personalAvatarSource().image : ''));

    function onMenu(event: MouseEvent) {
      emit('menu', { event, message: props.message });
    }
    return { onMenu, formatBytes, avatarSeed, avatarName, avatarImage };
  }
});
</script>
