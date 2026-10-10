<!--
  执行状态卡片：执行型事务八态状态机（§6.2-3）。
  状态由内核从操作集合 + 链上锚定时刻确定性推导，未锚定如实呈现。
-->
<template>
  <el-card shadow="never">
    <template #header>
      <h4>执行状态（{{ execStates.length }}）</h4>
    </template>
    <div v-for="state in execStates" :key="state.resolutionOpHash" class="op-item">
      <div class="op-meta">
        <el-tag size="small" :type="execStateTagType(state.state)">{{ execStateText(state.state) }}</el-tag>
        <span>决议 {{ shortId(state.resolutionOpHash) }}</span>
        <span v-if="state.reportOpHash">回报 {{ shortId(state.reportOpHash) }}</span>
      </div>
      <p class="hint">
        {{ state.effectiveMs === null ? '决议未生效' : `生效于 ${formatDate(state.effectiveMs)}` }}
      </p>
    </div>
    <p class="hint">执行型事务状态机（§6.2-3）由内核从操作集合 + 链上锚定时刻确定性推导，未锚定如实呈现。</p>
  </el-card>
</template>

<script setup lang="ts">
import type { ExecStateName, ExecStateView } from './model';
import { formatDate, shortId } from './view-text';

defineProps<{
  execStates: ExecStateView[];
}>();

/** 执行状态八态中文文案（§6.2-3；与内核 ExecState 线形逐字对齐） */
function execStateText(state: ExecStateName): string {
  return {
    unanchored: '未锚定',
    'resolution-pending': '决议公示中',
    'resolution-vetoed': '决议被否决',
    'awaiting-execution': '待执行',
    'in-progress': '执行中',
    verifying: '核查中',
    returned: '已打回',
    closed: '已关闭'
  }[state];
}

function execStateTagType(state: ExecStateName): 'info' | 'success' | 'danger' | 'warning' | 'primary' {
  return ({
    unanchored: 'info',
    'resolution-pending': 'warning',
    'resolution-vetoed': 'danger',
    'awaiting-execution': 'warning',
    'in-progress': 'primary',
    verifying: 'warning',
    returned: 'danger',
    closed: 'success'
  } as const)[state];
}
</script>

<style scoped>
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
</style>
