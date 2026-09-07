<!--
  公共议题客户端（spark-affairs）· message-card 视图：议题卡片。

  承载于应用消息（card.viewId='affair-card'，card.data={affairId}）：
  只渲染议题元数据 + 操作计数摘要——卡片数据随应用消息本地落库，正文
  永远经 sdk.affairs 查询，不随消息冗余（同 spark-example 帖子卡片的纪律）。
-->
<template>
  <div class="affair-card" v-loading="loading">
    <template v-if="detail">
      <p class="card-title">{{ detail.title }}</p>
      <p class="card-summary">{{ detail.summary }}</p>
      <p class="card-meta">
        {{ detail.originator.slice(0, 12) }}… · {{ detail.operationCount }} 条操作
        <el-tag size="small" :type="detail.closed ? 'info' : 'success'">
          {{ detail.closed ? '已关闭' : '进行中' }}
        </el-tag>
      </p>
    </template>
    <p v-else-if="!loading" class="card-summary">议题加载失败（可能已无人持有副本）。</p>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import { AffairsService } from './service';
import type { AffairDetail } from './model';

const props = defineProps<{
  cardData?: { affairId?: string };
}>();

const loading = ref(true);
const detail = ref<AffairDetail | null>(null);

onMounted(async () => {
  const affairId = props.cardData?.affairId;
  if (!affairId) {
    loading.value = false;
    return;
  }
  try {
    const sdk = await ensurePluginSDK();
    if (AffairsService.isAvailable(sdk)) {
      detail.value = await new AffairsService(sdk).getDetail(affairId);
    }
  } catch {
    detail.value = null;
  } finally {
    loading.value = false;
  }
});
</script>

<style scoped>
.affair-card {
  padding: 4px 2px;
}
.card-title {
  margin: 0 0 4px;
  font-weight: 600;
  font-size: 14px;
}
.card-summary {
  margin: 0 0 4px;
  font-size: 12px;
  color: var(--el-text-color-regular);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.card-meta {
  margin: 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  display: flex;
  align-items: center;
  gap: 6px;
}
</style>
