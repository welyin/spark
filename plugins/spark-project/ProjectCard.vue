<template>
  <section class="project-card">
    <header class="card-header">
      <span class="card-badge">{{ badge }}</span>
      <span class="card-title">{{ cardData?.title || '（未命名）' }}</span>
    </header>
    <p class="card-hint">
      事务 {{ shortId }}… —— 详情在「项目」插件中查看；未安装本插件的成员可见本条纯文本摘要。
    </p>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { DISPOSITION_LABELS } from './model';

/** 项目动态卡片（message-card）：创建回执 / 子事务处置通知，data 只携带引用 */
export default defineComponent({
  name: 'ProjectCard',
  props: {
    cardData: {
      type: Object as () =>
        | { kind?: 'project-created' | 'child-disposition'; affairId?: string; title?: string; action?: string }
        | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const badge = computed(() => {
      if (props.cardData?.kind === 'child-disposition') {
        const action = props.cardData.action as keyof typeof DISPOSITION_LABELS | undefined;
        return `子事务${DISPOSITION_LABELS[action ?? 'closed'] ?? '处置'}`;
      }
      return '项目已创建';
    });
    const shortId = computed(() => (props.cardData?.affairId ?? '').slice(0, 12));
    return { badge, shortId };
  }
});
</script>

<style scoped>
.project-card {
  padding: 12px 14px;
  font-size: 13px;
  color: var(--el-text-color-primary, #303133);
}
.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}
.card-badge {
  flex: none;
  padding: 2px 8px;
  border-radius: 4px;
  background: var(--el-color-primary-light-8, #ecf5ff);
  color: var(--el-color-primary, #409eff);
  font-size: 12px;
}
.card-title {
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.card-hint {
  margin: 8px 0 0;
  color: var(--el-text-color-secondary, #909399);
  font-size: 12px;
}
</style>
