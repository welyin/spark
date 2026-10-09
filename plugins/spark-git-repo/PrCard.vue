<!--
  代码仓库应用（spark-git-repo）· PR 卡片（message-card 视图）。

  「消息自描述」约定：card.data 自带 title/status/base 摘要字段，卡片不依赖
  事务读取即可呈现；事务可读时再从子事务日志补最新状态（append-only 推导）。
-->
<template>
  <section class="pr-card">
    <div class="card-head">
      <span class="badge" :data-status="status">{{ statusText }}</span>
      <strong class="title">{{ title }}</strong>
    </div>
    <p v-if="base" class="meta">base: {{ base }}</p>
    <p v-if="detail" class="meta">
      修订 {{ detail.updates.length }} 次 · 评审 {{ detail.reviews.length }} 条 · 评论 {{ detail.comments.length }} 条
    </p>
    <p v-if="detail?.merged" class="meta ok">
      合并回执：{{ detail.merged.resultCommit.slice(0, 12) }}… → 镜像 v{{ detail.merged.mirrorVersion }}
    </p>
    <p class="hint">在「代码仓库」应用中查看完整评审流与附件检出指引</p>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { GitRepoService } from './service';
import { buildPrSummary, type PrState } from './model';

const props = defineProps<{
  cardData?: { affairId?: string; title?: string; status?: string; base?: string };
}>();

const detail = ref<PrState | null>(null);

const title = computed(() => detail.value?.open?.title ?? props.cardData?.title ?? '(PR)');
const base = computed(() => detail.value?.open?.base ?? props.cardData?.base ?? '');
const status = computed(() => detail.value?.status ?? 'open');
const statusText = computed(() => {
  if (props.cardData?.status && !detail.value) {
    return props.cardData.status;
  }
  return buildPrSummary({ open: detail.value?.open ?? null, status: status.value }).split('—')[1]?.trim() ?? '';
});

onMounted(async () => {
  const sdk = window.__sparkPluginSDK;
  const affairId = props.cardData?.affairId;
  if (!sdk || !affairId) {
    return;
  }
  try {
    const service = new GitRepoService(sdk);
    detail.value = await service.getPrDetail(affairId);
  } catch {
    // 子事务未同步/不可读：卡片按自带摘要呈现（消息自描述），不报错
  }
});
</script>

<style scoped>
.pr-card {
  padding: 10px 12px;
  font-size: 13px;
  line-height: 1.6;
}
.card-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.badge {
  flex: none;
  padding: 1px 8px;
  border-radius: 10px;
  font-size: 12px;
  background: #e6f4ff;
  color: #1677ff;
}
.badge[data-status='merged'] {
  background: #f6ffed;
  color: #52c41a;
}
.badge[data-status='closed'] {
  background: #fff1f0;
  color: #cf1322;
}
.title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.meta {
  margin: 2px 0 0;
  color: #666;
}
.meta.ok {
  color: #52c41a;
}
.hint {
  margin: 6px 0 0;
  color: #999;
  font-size: 12px;
}
</style>
