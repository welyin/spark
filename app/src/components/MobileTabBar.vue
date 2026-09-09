<!-- 移动端底部 tab 导航（窄屏 ≤768px 时替代左侧 rail，由 App.vue 按 ui-layout 断点渲染）：
     新 UI 四一级入口 消息/空间/事务/我的（docs/ui/README §五），「我的」固定最右；
     激活态与 rail 同一状态源（App.vue activeTab）；
     底部 padding 吃 env(safe-area-inset-bottom)，内容避开 Android 手势导航条（桌面端 env() 为 0） -->
<template>
  <nav class="mobile-tab-bar">
    <button
      v-for="tab in tabs"
      :key="tab.id"
      class="mobile-tab-item"
      :class="{ active: activeTab === tab.id }"
      @click="emit('select', tab.id)"
    >
      <!-- 与 rail 一致：消息入口挂未读角标（>99 显示 99+） -->
      <el-badge v-if="tab.id === 'messages'" :value="messagesBadge" :max="99" :hidden="messagesBadge === 0">
        <el-icon :size="22"><component :is="tab.icon" /></el-icon>
      </el-badge>
      <el-icon v-else :size="22"><component :is="tab.icon" /></el-icon>
      <span class="mobile-tab-label">{{ tab.label }}</span>
    </button>
  </nav>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { ChatDotRound, Document, Grid, User } from '@element-plus/icons-vue';
import { MOBILE_TABS } from '../stores/ui-layout';

export default defineComponent({
  name: 'MobileTabBar',
  components: { ChatDotRound, Grid, Document, User },
  props: {
    /** 当前激活 tab（App.vue activeTab，与 rail 同源；插件 tab 打开时无激活项） */
    activeTab: { type: String, required: true },
    messagesBadge: { type: Number, default: 0 }
  },
  emits: ['select'],
  setup(_, { emit }) {
    // 图标映射留在组件内：ui-layout 保持纯逻辑（tab 定义）便于单测；
    // emit 必须从 setup 上下文解构返回，模板里才能用 emit('select', id)
    // （裸 setup() 时模板中的 emit 是 undefined，点击静默无效）
    const icons = { messages: ChatDotRound, space: Grid, affairs: Document, mine: User };
    const tabs = MOBILE_TABS.map((tab) => ({ ...tab, icon: icons[tab.id] }));
    return { tabs, emit };
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
