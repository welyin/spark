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
          @contextmenu.prevent="onContextMenu($event, conv)"
          @touchstart="lp.start($event, conv)"
          @touchmove="lp.move"
          @touchend="lp.end"
          @touchcancel="lp.end"
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

    <!-- 右键菜单（G3：弹层体系统一 Element dropdown，virtual-ref 锚定被点行；
         原手写 ctx-menu teleport 已移除） -->
    <el-dropdown
      ref="menuRef"
      trigger="contextmenu"
      virtual-triggering
      :virtual-ref="menuAnchor"
      placement="bottom-start"
      popper-class="spark-ctx-popper"
      @command="onMenuCommand"
      @visible-change="onMenuVisibleChange"
    >
      <span class="conv-menu-anchor" aria-hidden="true" />
      <template #dropdown>
        <el-dropdown-menu>
          <el-dropdown-item command="pin" :icon="Top">
            {{ menu.conv?.pinnedAt ? '取消置顶' : '置顶聊天' }}
          </el-dropdown-item>
          <el-dropdown-item command="mute" :icon="MuteNotification">
            {{ menu.conv?.muted ? '取消免打扰' : '消息免打扰' }}
          </el-dropdown-item>
          <!-- 应用会话：屏蔽为本地持久化状态（抑制未读角标与聚合，列表仍可见可取消） -->
          <el-dropdown-item v-if="menu.conv?.kind === 'app'" command="block" :icon="Remove">
            {{ menu.conv && isBlockedApp(menu.conv) ? '取消屏蔽' : '屏蔽应用消息' }}
          </el-dropdown-item>
          <!-- 应用消息内核无「清空」接口（仅删除会话），应用会话不展示清空项 -->
          <el-dropdown-item v-if="menu.conv?.kind !== 'app'" command="clear" :icon="Brush" divided class="ctx-item-danger">
            清空聊天记录
          </el-dropdown-item>
          <el-dropdown-item command="delete" :icon="Delete" :divided="menu.conv?.kind === 'app'" class="ctx-item-danger">
            删除会话
          </el-dropdown-item>
        </el-dropdown-menu>
      </template>
    </el-dropdown>
  </aside>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, reactive, ref } from 'vue';
import { ElMessageBox, type DropdownInstance } from 'element-plus';
import { BellFilled, Brush, Delete, MuteNotification, Remove, Search, Top } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import {
  appConversationName,
  isAppConversationBlocked,
  toggleAppConversationBlocked
} from '../../stores/app-conversations';
import { hashGradient } from '../../utils/palette';
import { createLongPress } from '../mobile-long-press';
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
} from '../../stores/messages';

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

    // 空状态（设计 §2.4）：区分无会话与搜索无结果。
    // 消息 = 统一收件箱（私聊 + 应用/系统通知卡片流），不承诺“只有聊天”
    const emptyText = computed(() => {
      if (keyword.value.trim()) return '未找到相关会话';
      return '暂无消息：聊天与应用通知会在这里出现';
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

    // 右键菜单状态（G3：Element dropdown virtual-ref 锚定；conv 为当前菜单目标会话）
    const menu = reactive<{ conv: Conversation | null }>({ conv: null });
    const menuAnchor = ref<HTMLElement | null>(null);
    const menuRef = ref<DropdownInstance | null>(null);

    function openMenu(anchor: HTMLElement | null, conv: Conversation) {
      if (!anchor) return;
      menu.conv = conv;
      menuAnchor.value = anchor;
      // 等 anchor 更新后再开（首次打开时 virtual-ref 尚未指向目标行）
      void nextTick(() => menuRef.value?.handleOpen());
    }

    /** 桌面端右键：锚定被点行 */
    function onContextMenu(event: MouseEvent, conv: Conversation) {
      openMenu(event.currentTarget as HTMLElement, conv);
    }

    /** 移动端长按（M23：会话项次级操作——置顶/免打扰/删除等，与右键同一菜单） */
    const lp = createLongPress<Conversation>((conv, event) => {
      openMenu((event.target as HTMLElement).closest('.conv-item'), conv);
    });

    function onMenuVisibleChange(visible: boolean) {
      if (!visible) {
        menu.conv = null;
      }
    }

    /** 菜单命令分发（dropdown 选择后自动关闭，无需手动 close） */
    function onMenuCommand(command: string) {
      if (command === 'pin') onPin();
      else if (command === 'mute') onMute();
      else if (command === 'block') onBlock();
      else if (command === 'clear') void onClear();
      else if (command === 'delete') void onDelete();
    }

    function onPin() {
      if (menu.conv) togglePin(props.spaceKey, menu.conv.id);
    }

    function onMute() {
      if (menu.conv) toggleMute(props.spaceKey, menu.conv.id);
    }

    /** 屏蔽/取消屏蔽应用会话（本地持久化，stores/app-conversations） */
    function onBlock() {
      if (menu.conv?.kind === 'app') {
        toggleAppConversationBlocked(props.spaceKey, menu.conv.peerId);
      }
    }

    async function onClear() {
      const conv = menu.conv;
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
      menuAnchor,
      menuRef,
      peerAvatar,
      convName,
      appAvatarBg,
      appAvatarLetter,
      isBlockedApp,
      openMenu,
      onContextMenu,
      lp,
      onMenuCommand,
      onMenuVisibleChange,
      lastMessage,
      lastAppSummary,
      previewText,
      formatConvTime,
      Search,
      Top,
      MuteNotification,
      Remove,
      Brush,
      Delete,
      unreadLabel: (n: number) => (n > 99 ? '…' : String(n))
    };
  }
});
</script>
