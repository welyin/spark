<!-- 会话列表（设计 §2）：搜索、排序、未读红点、草稿、空状态、右键更多操作（§5.1） -->
<template>
  <aside class="conv-list">
    <div class="conv-search">
      <el-input v-model="keyword" placeholder="搜索" clearable :prefix-icon="Search" />
    </div>

    <div v-if="filtered.length" class="conv-scroll">
      <section v-for="section in sections" :key="section.label" class="conv-group">
        <p v-if="section.showLabel" class="conv-group-label">{{ section.label }}</p>
        <div
          v-for="conv in section.items"
          :key="conv.id"
          class="conv-item"
          :class="{ active: conv.id === activeId, pinned: conv.pinnedAt > 0 }"
          @click="$emit('select', conv.id)"
          @contextmenu.prevent="openMenu($event, conv)"
        >
          <div class="conv-avatar">
            <!-- 系统通知会话（含内置系统应用会话 app:system）用 Bell 图标头像区分；
                 应用会话用插件名首字 + pluginId 哈希渐变（与应用列表图标同口径）；
                 普通会话用对方头像 -->
            <span v-if="conv.kind === 'system' || (conv.kind === 'app' && conv.peerId === 'system')" class="conv-sys-avatar">
              <el-icon :size="20"><BellFilled /></el-icon>
            </span>
            <span
              v-else-if="conv.kind === 'app'"
              class="conv-app-avatar"
              :style="{ background: appAvatarBg(conv) }"
            >{{ appAvatarLetter(conv) }}</span>
            <UserAvatar v-else :root-id="conv.peerId" :nickname="convName(conv)" :avatar="peerAvatar(conv)" :size="40" />
          </div>
          <div class="conv-main">
            <div class="conv-line1">
              <span class="conv-name">{{ convName(conv) }}</span>
              <span v-if="isBlockedApp(conv)" class="conv-blocked-tag">已屏蔽</span>
            </div>
            <div class="conv-line2">
              <span class="conv-preview">
                <span v-if="conv.draft" class="conv-draft">[草稿] </span>
                <template v-else-if="conv.kind === 'app'">{{ lastAppSummary(spaceKey, conv.id) }}</template>
                <template v-else>{{ previewText(lastMessage(spaceKey, conv.id)) }}</template>
              </span>
            </div>
          </div>
          <!-- 右列：时间上、未读角标下，右对齐 -->
          <div class="conv-side">
            <span class="conv-time">{{ formatConvTime(conv.updatedAt) }}</span>
            <div class="conv-side-flags">
              <el-icon v-if="conv.muted" :size="13"><MuteNotification /></el-icon>
              <el-icon v-if="conv.pinnedAt > 0" :size="13"><Top /></el-icon>
              <span v-if="conv.unreadCount > 0 && !isBlockedApp(conv)" class="conv-badge" :class="{ muted: conv.muted }">
                {{ conv.muted ? '' : unreadLabel(conv.unreadCount) }}
              </span>
            </div>
          </div>
        </div>
      </section>
    </div>

    <el-empty v-else :image-size="90" :description="emptyText" class="conv-empty" />

    <teleport to="body">
      <div v-if="menu.visible" class="ctx-mask" @click="closeMenu" @contextmenu.prevent="closeMenu">
        <ul class="ctx-menu" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }">
          <li @click="onPin">
            <el-icon :size="14"><Top /></el-icon>
            {{ menu.conv?.pinnedAt ? '取消置顶' : '置顶聊天' }}
          </li>
          <li @click="onMute">
            <el-icon :size="14"><MuteNotification /></el-icon>
            {{ menu.conv?.muted ? '取消免打扰' : '消息免打扰' }}
          </li>
          <!-- 应用会话：屏蔽为本地持久化状态（抑制未读角标与聚合，列表仍可见可取消） -->
          <li v-if="menu.conv?.kind === 'app'" @click="onBlock">
            <el-icon :size="14"><Remove /></el-icon>
            {{ menu.conv && isBlockedApp(menu.conv) ? '取消屏蔽' : '屏蔽应用消息' }}
          </li>
          <li class="ctx-divider" />
          <!-- 应用消息内核无「清空」接口（仅删除会话），应用会话不展示清空项 -->
          <li v-if="menu.conv?.kind !== 'app'" class="danger" @click="onClear">
            <el-icon :size="14"><Brush /></el-icon>
            清空聊天记录
          </li>
          <li class="danger" @click="onDelete">
            <el-icon :size="14"><Delete /></el-icon>
            删除会话
          </li>
        </ul>
      </div>
    </teleport>
  </aside>
</template>

<script lang="ts">
import { computed, defineComponent, reactive, ref } from 'vue';
import { ElMessageBox } from 'element-plus';
import { BellFilled, Brush, Delete, MuteNotification, Remove, Search, Top } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import {
  appConversationName,
  isAppConversationBlocked,
  toggleAppConversationBlocked
} from '../../stores/app-conversations';
import { hashGradient } from '../../utils/palette';
import {
  clearMessages,
  deleteConversation,
  lastAppSummary,
  lastMessage,
  listConversations,
  previewText,
  toggleMute,
  togglePin,
  formatConvTime,
  type Conversation,
  type SpaceKey
} from '../../mock/messages';

export default defineComponent({
  name: 'ConversationList',
  components: { UserAvatar, MuteNotification, Top, BellFilled, Brush, Delete, Remove },
  props: {
    spaceKey: { type: String as () => SpaceKey, required: true },
    spaceType: { type: String as () => 'personal' | 'org', required: true },
    activeId: { type: String, default: '' }
  },
  emits: ['select', 'removed'],
  setup(props, { emit }) {
    const keyword = ref('');

    const sorted = computed(() => listConversations(props.spaceKey));

    // direct 会话的 peerId 即对方 rootId：统一头像入口（朋友记录优先），无则走自动头像
    function peerAvatar(conv: Conversation): string {
      return conv.kind === 'direct' ? personAvatarSource(props.spaceKey, conv.peerId).image : '';
    }

    // 会话名：direct 走统一展示名入口（备注>昵称>原标题），改备注后列表/搜索同步生效；
    // app 走插件清单名称（缺省 pluginId，内核会话标题的缺省值即 pluginId）
    function convName(conv: Conversation): string {
      if (conv.kind === 'app') {
        return appConversationName(conv.peerId, conv.title);
      }
      return conv.kind === 'direct' ? personDisplayName(props.spaceKey, conv.peerId, conv.title) : conv.title;
    }

    // 应用会话头像：插件名首字 + pluginId 哈希渐变（与应用列表 appIconBackground 同口径）
    function appAvatarBg(conv: Conversation): string {
      return hashGradient(conv.peerId);
    }
    function appAvatarLetter(conv: Conversation): string {
      return convName(conv).slice(0, 1);
    }

    /** 应用会话屏蔽态（本地持久化；抑制未读角标，列表仍可见可取消） */
    function isBlockedApp(conv: Conversation): boolean {
      return conv.kind === 'app' && isAppConversationBlocked(props.spaceKey, conv.peerId);
    }

    // 按名称/最新消息内容模糊搜索（设计 §2.5；应用会话匹配最新摘要）
    const filtered = computed(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) return sorted.value;
      return sorted.value.filter((conv) => {
        if (convName(conv).toLowerCase().includes(kw)) return true;
        const preview = conv.kind === 'app' ? lastAppSummary(props.spaceKey, conv.id) : previewText(lastMessage(props.spaceKey, conv.id));
        return preview.toLowerCase().includes(kw);
      });
    });

    // 空状态（设计 §2.4）：区分无会话与搜索无结果
    const emptyText = computed(() => {
      if (keyword.value.trim()) return '未找到相关会话';
      return props.spaceType === 'org' ? '组织消息会出现在这里' : '添加朋友后开始聊天';
    });

    // 系统会话/应用会话固定顶部与单聊分组展示，不混排（应用会话=服务号模型 §20）
    const sections = computed(() => {
      const groups = [
        { label: '系统通知', items: filtered.value.filter((conv) => conv.kind === 'system') },
        { label: '应用', items: filtered.value.filter((conv) => conv.kind === 'app') },
        { label: '单聊', items: filtered.value.filter((conv) => conv.kind === 'direct') }
      ].filter((group) => group.items.length > 0);
      const showLabel = groups.length > 1;
      return groups.map((group) => ({ ...group, showLabel }));
    });

    const menu = reactive<{ visible: boolean; x: number; y: number; conv: Conversation | null }>({
      visible: false,
      x: 0,
      y: 0,
      conv: null
    });

    function openMenu(event: MouseEvent, conv: Conversation) {
      menu.visible = true;
      menu.x = event.clientX;
      menu.y = event.clientY;
      menu.conv = conv;
    }

    function closeMenu() {
      menu.visible = false;
      menu.conv = null;
    }

    function onPin() {
      if (menu.conv) togglePin(props.spaceKey, menu.conv.id);
      closeMenu();
    }

    function onMute() {
      if (menu.conv) toggleMute(props.spaceKey, menu.conv.id);
      closeMenu();
    }

    /** 屏蔽/取消屏蔽应用会话（本地持久化，stores/app-conversations） */
    function onBlock() {
      if (menu.conv?.kind === 'app') {
        toggleAppConversationBlocked(props.spaceKey, menu.conv.peerId);
      }
      closeMenu();
    }

    async function onClear() {
      const conv = menu.conv;
      closeMenu();
      if (!conv) return;
      try {
        await ElMessageBox.confirm('仅删除本地消息记录，不影响对方设备。', `清空与「${convName(conv)}」的聊天记录？`, {
          confirmButtonText: '清空',
          cancelButtonText: '取消',
          type: 'warning'
        });
        clearMessages(props.spaceKey, conv.id);
      } catch {
        // 用户取消
      }
    }

    async function onDelete() {
      const conv = menu.conv;
      closeMenu();
      if (!conv) return;
      try {
        await ElMessageBox.confirm('仅删除会话列表入口。', `删除与「${convName(conv)}」的会话？`, {
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

    return {
      keyword,
      filtered,
      sections,
      emptyText,
      menu,
      peerAvatar,
      convName,
      appAvatarBg,
      appAvatarLetter,
      isBlockedApp,
      openMenu,
      closeMenu,
      onPin,
      onMute,
      onBlock,
      onClear,
      onDelete,
      lastMessage,
      lastAppSummary,
      previewText,
      formatConvTime,
      Search,
      unreadLabel: (n: number) => (n > 99 ? '…' : String(n))
    };
  }
});
</script>
