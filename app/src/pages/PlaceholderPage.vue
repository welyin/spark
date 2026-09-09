<!-- 占位页（阶段 0）：新一级入口（空间/事务）的"建设中"空状态。
     出处 docs/ui/todo 0.4——保证一级入口换血（0.2）后各 tab 即可发布，
     正式形态按阶段 1/3 补齐（空间=桌面，事务=跨域列表）。 -->
<template>
  <div class="placeholder-page">
    <el-empty :image-size="120" :description="`${title}建设中`">
      <template #default>
        <p class="placeholder-hint">{{ hint }}</p>
      </template>
    </el-empty>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';

export default defineComponent({
  name: 'PlaceholderPage',
  props: {
    title: { type: String, required: true },
    hint: { type: String, default: '' }
  }
});
</script>

<style scoped>
.placeholder-page {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--spark-bg-page);
}

.placeholder-hint {
  margin: 0;
  max-width: 320px;
  text-align: center;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  line-height: 1.6;
}
</style>
