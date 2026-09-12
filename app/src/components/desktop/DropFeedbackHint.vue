<!-- 拖放态跟随光标提示（problem X4）：拖拽（壳层对象 / OS 文件拖入）期间，
     松手前始终看得到「进哪个域、谁可见」；不可承接处显示「不可承接」并配合全局禁止光标。 -->
<template>
  <Teleport to="body">
    <div
      v-if="hint.visible"
      class="spark-drop-hint"
      :class="{ 'spark-drop-hint--denied': !hint.allowed }"
      :style="{ left: `${hint.x + 14}px`, top: `${hint.y + 14}px` }"
    >
      <template v-if="hint.allowed">
        <span class="spark-drop-hint-target">目标域：{{ hint.targetLabel }}</span>
        <span class="spark-drop-hint-scope">{{ hint.scopeLabel }}</span>
      </template>
      <span v-else class="spark-drop-hint-target">此处不可承接</span>
    </div>
  </Teleport>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { dragSession, scopeLabelOf, spaceLabelOf } from '../../stores/cross-domain-drop';
import { osDropState } from '../../stores/os-file-drop';

export default defineComponent({
  name: 'DropFeedbackHint',
  setup() {
    /** 统一两类拖拽（壳层对象 / OS 文件）的提示数据；OS 拖入优先（两者不会同时发生） */
    const hint = computed(() => {
      const os = osDropState.value;
      if (os.active) {
        return {
          visible: true,
          x: os.x,
          y: os.y,
          allowed: os.allowed && !!os.target,
          targetLabel: os.target ? spaceLabelOf(os.target) : '',
          scopeLabel: os.target ? scopeLabelOf(os.target) : ''
        };
      }
      const drag = dragSession.value;
      if (drag) {
        return {
          visible: true,
          x: drag.x,
          y: drag.y,
          allowed: drag.hoverAllowed && !!drag.hover,
          targetLabel: drag.hover ? spaceLabelOf(drag.hover) : '',
          scopeLabel: drag.hover ? scopeLabelOf(drag.hover) : ''
        };
      }
      return { visible: false, x: 0, y: 0, allowed: false, targetLabel: '', scopeLabel: '' };
    });
    return { hint };
  }
});
</script>

<style scoped>
.spark-drop-hint {
  position: fixed;
  z-index: var(--spark-z-notify);
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 6px 10px;
  border-radius: var(--spark-radius-m);
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-primary);
  box-shadow: var(--spark-shadow-pop);
  pointer-events: none;
  max-width: 260px;
}

.spark-drop-hint--denied {
  border-color: var(--spark-border-light);
}

.spark-drop-hint-target {
  font-size: var(--spark-font-size-secondary);
  font-weight: 600;
  color: var(--spark-text-1);
}

.spark-drop-hint-scope {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}
</style>
