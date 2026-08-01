<!-- 移动端返回栏（移动端适配波次 2）：窄屏（≤768px）下栈深 >1 的整页顶部显示「‹ 返回 + 页面标题」，
     栈深 1 不渲染（由父级按 canBack 控制）；桌面端不渲染本组件。
     样式复用现有 token 体系（--spark-*），高度与聊天头（.chat-header）同档 -->
<template>
  <header class="mobile-back-bar">
    <button type="button" class="mobile-back-btn" @click="emit('back')">
      <el-icon :size="18"><ArrowLeft /></el-icon>
      <span>返回</span>
    </button>
    <h2 class="mobile-back-title">{{ title }}</h2>
    <!-- 右侧占位：与左侧返回区同格宽，标题保持整栏居中 -->
    <span class="mobile-back-spacer" aria-hidden="true" />
  </header>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { ArrowLeft } from '@element-plus/icons-vue';

export default defineComponent({
  name: 'MobileBackBar',
  components: { ArrowLeft },
  props: {
    /** 页面标题（当前栈帧对应层的名称，如分组名/应用名/模块名） */
    title: { type: String, required: true }
  },
  emits: ['back'],
  setup(_, { emit }) {
    return { emit };
  }
});
</script>

<style scoped>
.mobile-back-bar {
  flex-shrink: 0;
  display: grid;
  grid-template-columns: 1fr auto 1fr;
  align-items: center;
  height: 48px;
  background: var(--spark-bg-card);
  border-bottom: 1px solid var(--spark-border-light);
}

.mobile-back-btn {
  justify-self: start;
  display: flex;
  align-items: center;
  gap: 2px;
  height: 100%;
  margin: 0;
  padding: 0 12px;
  border: 0;
  background: transparent;
  font-family: inherit;
  font-size: 14px;
  color: var(--spark-primary);
  cursor: pointer;
}

.mobile-back-title {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--spark-text-1);
  max-width: 50vw;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
