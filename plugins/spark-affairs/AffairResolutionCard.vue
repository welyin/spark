<!--
  决议卡片：决议计划预览（关闭条件满足判定 + 名册过滤计票）与决议列表。
  决议有效性由内核逐副本复算把关（condition ∈ 规则版本 + countedOps 匹配 +
  公示期一致），复算不符进 invalidResolutions 如实呈现，客户端不伪造。
-->
<template>
  <el-card shadow="never">
    <template #header>
      <div class="header-row">
        <h4>决议（{{ resolutions.length }}）</h4>
        <el-button v-if="!closed" size="small" type="primary" plain
          :disabled="!resolutionPlan" :loading="resolving" @click="$emit('submit')">
          发起决议（结案）
        </el-button>
      </div>
    </template>
    <p class="hint" v-if="!closed">
      <template v-if="resolutionPlan">
        当前计票（阶梯名册内一人一票，非投票者票不计入）：赞成 {{ resolutionPlan.tally.for }} /
        反对 {{ resolutionPlan.tally.against }} / 弃权 {{ resolutionPlan.tally.abstain }}。
        关闭条件已满足（投票操作达法定人数 {{ minQuorum }}），可发起决议——
        按当前计票结果为「{{ resolutionPlan.result === 'passed' ? '通过' : '未通过' }}」；
        决议落日志后进入公示期，内核逐副本复算关闭条件与计入操作，复算不符将如实标为无效决议。
      </template>
      <template v-else>
        关闭条件未满足：本议题的投票操作（针对任意贡献的赞成/反对/弃权票）达到法定人数
        {{ minQuorum }} 条后可发起决议。2026-10-07 前创建的旧议题未声明关闭条件，
        无法产生决议。
      </template>
    </p>
    <div v-for="resolution in resolutions" :key="resolution.opHash" class="op-item">
      <div class="op-meta">
        <el-tag size="small" :type="resolutionTagType(resolution.state)">{{ resolutionStateText(resolution.state) }}</el-tag>
        <span>{{ resolution.opHash.slice(0, 12) }}…</span>
      </div>
      <p class="op-content">{{ resolution.resultText }}</p>
      <p class="hint">
        异议 {{ resolution.objections }} 条 · 公示期 {{ Math.round(resolution.pubPeriodMs / 3600000) }} 小时 ·
        {{ resolution.anchoredMs === null ? '未锚定（不以声明时间冒充链上时间）' : `锚定于 ${formatDate(resolution.anchoredMs)}` }}
      </p>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import type { AffairResolutionState, AffairResolutionView, ResolutionPlan } from './model';
import { formatDate } from './view-text';

defineProps<{
  resolutions: AffairResolutionView[];
  /** 决议计划（关闭条件满足判定 + 计票预览；null = 暂不能发起决议） */
  resolutionPlan: ResolutionPlan | null;
  closed: boolean;
  minQuorum: number;
  resolving: boolean;
}>();

defineEmits<{
  (event: 'submit'): void;
}>();

function resolutionStateText(state: AffairResolutionState): string {
  return { pending: '公示中', effective: '已生效', vetoed: '已否决', unanchored: '未锚定' }[state];
}

function resolutionTagType(state: AffairResolutionState): 'info' | 'success' | 'danger' | 'warning' {
  return ({ pending: 'warning', effective: 'success', vetoed: 'danger', unanchored: 'info' } as const)[state];
}
</script>

<style scoped>
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  row-gap: 4px;
}
.hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.op-item {
  padding: 10px 0;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.op-item:last-child {
  border-bottom: none;
}
.op-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  flex-wrap: wrap;
}
.op-content {
  margin: 6px 0;
  font-size: 13px;
  white-space: pre-wrap;
}
</style>
