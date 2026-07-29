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
            <!-- 系统通知会话用 Bell 图标头像区分，普通会话用对方头像 -->
            <span v-if="conv.kind === 'system'" class="conv-sys-avatar">
              <el-icon :size="20"><BellFilled /></el-icon>
            </span>
            <UserAvatar v-else :root-id="conv.peerId" :nickname="conv.title" :size="40" />
          </div>
          <div class="conv-main">
            <div class="conv-line1">
              <span class="conv-name">{{ conv.title }}</span>
            </div>
            <div class="conv-line2">
              <span class="conv-preview">
                <span v-if="conv.draft" class="conv-draft">[草稿] </span>
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
              <span v-if="conv.unreadCount > 0" class="conv-badge" :class="{ muted: conv.muted }">
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
          <li class="ctx-divider" />
          <li class="danger" @click="onClear">
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
import { BellFilled, Brush, Delete, MuteNotification, Search, Top } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import {
  clearMessages,
  deleteConversation,
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
  components: { UserAvatar, MuteNotification, Top, BellFilled, Brush, Delete },
  props: {
    spaceKey: { type: String as () => SpaceKey, required: true },
    spaceType: { type: String as () => 'personal' | 'org', required: true },
    activeId: { type: String, default: '' }
  },
  emits: ['select', 'removed'],
  setup(props, { emit }) {
    const keyword = ref('');

    const sorted = computed(() => listConversations(props.spaceKey));

    // 按名称/最新消息内容模糊搜索（设计 §2.5）
    const filtered = computed(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) return sorted.value;
      return sorted.value.filter((conv) => {
        if (conv.title.toLowerCase().includes(kw)) return true;
        return previewText(lastMessage(props.spaceKey, conv.id)).toLowerCase().includes(kw);
      });
    });

    // 空状态（设计 §2.4）：区分无会话与搜索无结果
    const emptyText = computed(() => {
      if (keyword.value.trim()) return '未找到相关会话';
      return props.spaceType === 'org' ? '组织消息会出现在这里' : '添加朋友后开始聊天';
    });

    // 系统会话（系统通知/组织公告）固定顶部与单聊分组展示，不混排
    const sections = computed(() => {
      const groups = [
        { label: '系统通知', items: filtered.value.filter((conv) => conv.kind === 'system') },
        { label: '单聊', items: filtered.value.filter((conv) => conv.kind !== 'system') }
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

    async function onClear() {
      const conv = menu.conv;
      closeMenu();
      if (!conv) return;
      try {
        await ElMessageBox.confirm('仅删除本地消息记录，不影响对方设备。', `清空与「${conv.title}」的聊天记录？`, {
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
        await ElMessageBox.confirm('仅删除会话列表入口。', `删除与「${conv.title}」的会话？`, {
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
      openMenu,
      closeMenu,
      onPin,
      onMute,
      onClear,
      onDelete,
      lastMessage,
      previewText,
      formatConvTime,
      Search,
      unreadLabel: (n: number) => (n > 99 ? '…' : String(n))
    };
  }
});
</script>
