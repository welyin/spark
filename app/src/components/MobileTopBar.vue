<!-- 移动端顶部导航（Android 前端改造）：
     左侧=菜单图标（打开左滑侧边栏 MobileSpaceDrawer）；中间=当前页名（消息/通讯录/应用/我的）+
     页名右侧紧贴网络状态点（点击直达系统设置→网络状态）；右侧对称占位保证标题居中。
     仅在四个主 tab 且栈深=1（currentPage(tab).page==='root'）时由 App.vue 渲染，
     进入二级页（聊天/详情等）时顶部导航整体滑走隐藏（App.vue Transition）。 -->
<template>
  <header class="mobile-top-bar">
    <button type="button" class="mobile-top-bar-btn" title="切换空间" @click="emit('open-drawer')">
      <el-icon :size="20"><Menu /></el-icon>
    </button>

    <!-- 中间：页名 + 网络状态点（点紧贴文字右侧，随文字长度自适应） -->
    <div class="mobile-top-bar-title">
      <span class="mobile-top-bar-title-text">{{ title }}</span>
      <NetworkStatusBar variant="dot" @open-network-status="emit('open-network-status')" />
    </div>

    <!-- 右侧占位：与左侧同宽，标题保持整栏居中 -->
    <span class="mobile-top-bar-spacer" aria-hidden="true" />
  </header>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { Menu } from '@element-plus/icons-vue';
import NetworkStatusBar from './NetworkStatusBar.vue';

export default defineComponent({
  name: 'MobileTopBar',
  components: { Menu, NetworkStatusBar },
  props: {
    /** 当前页名（消息/通讯录/应用/我的） */
    title: { type: String, required: true }
  },
  emits: ['open-drawer', 'open-network-status'],
  setup(_, { emit }) {
    return { emit };
  }
});
</script>

<style scoped>
.mobile-top-bar {
  flex-shrink: 0;
  display: grid;
  grid-template-columns: 1fr auto 1fr;
  align-items: center;
  width: 100%;
  height: 100%;
}

.mobile-top-bar-btn {
  justify-self: start;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  margin: 0;
  padding: 0;
  border: 0;
  background: transparent;
  border-radius: var(--spark-radius-m);
  color: var(--spark-text-1);
  cursor: pointer;
  font-family: inherit;
  -webkit-app-region: no-drag;
}

.mobile-top-bar-btn:hover {
  background: var(--spark-bg-hover);
}

/* 中间：页名 + 网络点同一行，点紧贴文字右侧（gap 小、不固定宽度） */
.mobile-top-bar-title {
  justify-self: center;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  max-width: 100%;
}

.mobile-top-bar-title-text {
  font-size: 16px;
  font-weight: 600;
  color: var(--spark-text-1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mobile-top-bar-spacer {
  justify-self: end;
  width: 36px;
  flex-shrink: 0;
}
</style>
