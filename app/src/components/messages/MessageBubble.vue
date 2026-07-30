<!-- 消息气泡（设计 §3.2/§4）：文本/图片/文件/链接/语音卡片、引用块、发送状态（§3.3） -->
<template>
  <div class="msg-row" :class="{ mine: isMine }">
    <div class="msg-avatar">
      <!-- 自己的消息：头像/昵称/配色种子统一走 avatar-sources（与其它位置的个人头像一致，
           mock 的 senderId 'me' 会得到不同哈希色，不能直接用） -->
      <UserAvatar
        v-if="showAvatar"
        class="msg-avatar-clickable"
        :root-id="avatarSeed"
        :nickname="avatarName"
        :avatar="avatarImage"
        :size="36"
        @click="onAvatarClick"
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

        <!-- 链接预览卡片（设计 §6）：标题 + 图标 + 描述 + 来源；
             spark-org-invite:// 为组织邀请卡片，拦截跳转改为弹出确认抽屉 -->
        <a
          v-if="message.link"
          class="link-card"
          :href="message.link.url"
          target="_blank"
          rel="noreferrer"
          @click.stop="onLinkClick"
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
import { personalAvatarSource, personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import type { ChatMessage, SpaceKey } from '../../mock/messages';

/** 组织邀请链接协议（内核系统会话卡片消息：url = spark-org-invite://{inviteId}） */
const ORG_INVITE_SCHEME = 'spark-org-invite://';

export default defineComponent({
  name: 'MessageBubble',
  components: { UserAvatar, Picture, Document, Microphone, Link },
  props: {
    message: { type: Object as PropType<ChatMessage>, required: true },
    isMine: { type: Boolean, default: false },
    /** 同一分钟内连续消息合并头像（§3.2） */
    showAvatar: { type: Boolean, default: true },
    /** 所在空间：对方消息按 senderId(rootId) 查好友同步头像用 */
    spaceKey: { type: String as () => SpaceKey, required: true }
  },
  emits: ['menu', 'resend', 'avatar-click', 'org-invite-click'],
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
        // 统一展示名入口（备注>昵称>消息自报快照名）
        return personDisplayName(props.spaceKey, props.message.senderId, props.message.senderName);
      }
      const source = personalAvatarSource();
      return source.seed ? source.name : props.message.senderName;
    });
    // 对方消息：统一头像入口（senderId 即 rootId，朋友记录头像与聊天头/会话列表一致），
    // 未同步到头像时回退自动头像
    const avatarImage = computed(() => {
      if (props.isMine) {
        return personalAvatarSource().image;
      }
      return personAvatarSource(props.spaceKey, props.message.senderId).image;
    });

    function onMenu(event: MouseEvent) {
      emit('menu', { event, message: props.message });
    }

    // 点头像看资料卡：rootId 取现成口径（对方=senderId；自己=personalAvatarSource().seed 即 currentUser.rootId）
    function onAvatarClick() {
      emit('avatar-click', avatarSeed.value);
    }

    // 组织邀请卡片（系统会话 link 消息，url=spark-org-invite://{inviteId}，
    // link.domain=orgId）：拦截跳转，交给上层弹确认/拒绝抽屉；普通链接照原样新开
    function onLinkClick(event: MouseEvent) {
      const link = props.message.link;
      if (!link || !link.url.startsWith(ORG_INVITE_SCHEME)) {
        return;
      }
      event.preventDefault();
      emit('org-invite-click', {
        inviteId: link.url.slice(ORG_INVITE_SCHEME.length),
        orgId: link.domain,
        title: link.title,
        description: link.description
      });
    }
    return { onMenu, onAvatarClick, onLinkClick, formatBytes, avatarSeed, avatarName, avatarImage };
  }
});
</script>
