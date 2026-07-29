<!-- 设置开关组（mock）：统一渲染一组本地开关，供设置页各 mock 分组复用 -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>{{ title }}</h2>
    </template>
    <p v-if="hint" class="hint mock-group-hint">{{ hint }}</p>
    <!-- TODO(mock): 开关仅本地展示不生效，待对应设置项的后端/持久化方案落地 -->
    <div class="mock-group-rows">
      <div v-for="item in items" :key="item.key" class="mock-group-row">
        <span>{{ item.label }}</span>
        <el-switch v-model="states[item.key]" />
      </div>
    </div>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref, watch, type PropType } from 'vue';

export type MockSettingItem = {
  key: string;
  label: string;
};

export default defineComponent({
  name: 'MockSettingGroup',
  props: {
    title: { type: String, required: true },
    hint: { type: String, default: '' },
    items: { type: Array as PropType<MockSettingItem[]>, required: true }
  },
  setup(props) {
    const states = ref<Record<string, boolean>>({});
    watch(
      () => props.items,
      (items) => {
        states.value = Object.fromEntries(items.map((item) => [item.key, states.value[item.key] ?? false]));
      },
      { immediate: true }
    );
    return { states };
  }
});
</script>

<style scoped>
.mock-group-hint {
  margin-top: 0;
}

.mock-group-rows {
  display: flex;
  flex-direction: column;
  margin-top: 8px;
}

.mock-group-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 0;
  font-size: 13px;
  color: var(--spark-text-1);
}

.mock-group-row + .mock-group-row {
  border-top: 1px solid var(--spark-border-light);
}
</style>
