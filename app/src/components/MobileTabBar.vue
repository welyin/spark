<!-- 移动端底部 tab 导航（窄屏 ≤768px 时替代左侧 rail，由 App.vue 按 ui-layout 断点渲染）：
     五一级入口 消息/事务/空间/应用/设置（M4/M9，docs/ui/problem.md），「设置」固定最右；
     激活态与 rail 同一状态源（App.vue activeTab）；
     底部 padding 吃 env(safe-area-inset-bottom)，内容避开 Android 手势导航条（桌面端 env() 为 0）。
     M11 角标口径：消息=未读会话数（unreadConversationCountOf，免打扰/已屏蔽不计）、
     事务=「待我处理」数（affair-feed actionableCount，非全部进行中数）；
     角标由本组件直连 store 自算（App.vue 不再经 props 传入）。
     M13：再点一次当前 Tab——除 emit('select')（App.vue 重置该 tab 导航栈回列表页）外，
     另派发 spark:tab-reselect 事件，列表页据以滚动回顶并触发刷新（同下拉刷新数据源）。 -->
<template>
  <nav class="mobile-tab-bar">
    <button
      v-for="tab in tabs"
      :key="tab.id"
      class="mobile-tab-item"
      :class="{ active: activeTab === tab.id }"
      @click="onSelect(tab.id)"
    >
      <!-- M11：消息=未读会话数角标；事务=待我处理数角标（>99 显示 99+） -->
      <el-badge v-if="tab.id === 'messages'" :value="messagesBadge" :max="99" :hidden="messagesBadge === 0">
        <el-icon :size="22"><component :is="tab.icon" /></el-icon>
      </el-badge>
      <el-badge v-else-if="tab.id === 'affairs'" :value="affairsBadge" :max="99" :hidden="affairsBadge === 0">
        <el-icon :size="22"><component :is="tab.icon" /></el-icon>
      </el-badge>
      <el-icon v-else :size="22"><component :is="tab.icon" /></el-icon>
      <span class="mobile-tab-label">{{ tab.label }}</span>
    </button>
  </nav>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { Box, ChatDotRound, Document, Grid, Setting } from '@element-plus/icons-vue';
import { MOBILE_TABS } from '../stores/ui-layout';
import { unreadConversationCountOf } from '../stores/messages';
import { spaceKeyOf } from '../mock/contacts';
import { currentSpace } from '../stores/current-space';
import { actionableCount } from '../stores/affairs/affair-feed';

/** M13：重按当前 tab 事件（detail = tab id），列表页据以回顶 + 刷新 */
export const TAB_RESELECT_EVENT = 'spark:tab-reselect';

export default defineComponent({
  name: 'MobileTabBar',
  components: { ChatDotRound, Document, Grid, Box, Setting },
  props: {
    /** 当前激活 tab（App.vue activeTab，与 rail 同源；插件 tab 打开时无激活项） */
    activeTab: { type: String, required: true }
  },
  emits: ['select'],
  setup(props, { emit }) {
    // 图标映射留在组件内：ui-layout 保持纯逻辑（tab 定义）便于单测；
    // emit 必须从 setup 上下文解构返回，模板里才能用 emit('select', id)
    // （裸 setup() 时模板中的 emit 是 undefined，点击静默无效）
    const icons = { messages: ChatDotRound, affairs: Document, space: Grid, apps: Box, settings: Setting };
    const tabs = MOBILE_TABS.map((tab) => ({ ...tab, icon: icons[tab.id] }));

    // M11 角标口径：消息=未读会话数（不是未读消息总数）；事务=待我处理数（affair-feed 近似谓词）
    const messagesBadge = computed(() => unreadConversationCountOf(spaceKeyOf(currentSpace.value)));
    const affairsBadge = computed(() => actionableCount.value);

    const onSelect = (id: string) => {
      emit('select', id);
      // M13：再点当前 tab——App.vue 的 handleMenuSelect 已把该 tab 栈重置回列表页，
      // 这里补发 reselect 事件驱动「回顶部 + 刷新」（消息/事务页各自监听）
      if (id === props.activeTab) {
        window.dispatchEvent(new CustomEvent(TAB_RESELECT_EVENT, { detail: id }));
      }
    };

    return { tabs, messagesBadge, affairsBadge, onSelect };
  }
});
</script>

<style scoped>
.mobile-tab-bar {
  flex-shrink: 0;
  display: flex;
  background: var(--spark-rail-bg);
  border-top: 2px solid var(--spark-border-light);
  /* 安全区：手势导航条占位（桌面端 env() 恒为 0；Android 由 Kotlin 注入 --spark-safe-bottom） */
  padding-bottom: var(--spark-safe-bottom, env(safe-area-inset-bottom, 0px));
}

.mobile-tab-item {
  flex: 1;
  min-width: 0;
  min-height: 52px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  color: var(--spark-rail-text);
  transition: color 0.15s ease;
}

.mobile-tab-item.active {
  color: var(--spark-primary);
}

.mobile-tab-label {
  font-size: 11px;
  line-height: 1.2;
  white-space: nowrap;
}

/* 角标位置与 rail 同款修正：收回到图标右上角内侧 */
.mobile-tab-item :deep(.el-badge__content) {
  right: 0;
  transform: translate(20%, -50%);
  z-index: 2;
}
</style>
