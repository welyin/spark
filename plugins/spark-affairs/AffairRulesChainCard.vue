<!--
  规则版本链卡片：现行规则 + 已生效 rule-change 链 + 未生效条目归宿。
  每一版本由内核按规则修改机制确定性推导（§5.4），客户端只呈现。
-->
<template>
  <el-card shadow="never">
    <template #header>
      <h4>规则版本链（现行 v{{ rulesChain.currentSeq }} · {{ shortId(rulesChain.currentRulesHash) }}）</h4>
    </template>
    <el-table :data="rulesChain.versions" size="small">
      <el-table-column label="版本" width="70">
        <template #default="{ row }">v{{ row.seq }}</template>
      </el-table-column>
      <el-table-column label="依据">
        <template #default="{ row }">{{ row.seq === 0 ? '创世' : `rule-change ${shortId(row.basisOpHash)}` }}</template>
      </el-table-column>
      <el-table-column label="规则哈希">
        <template #default="{ row }">{{ shortId(row.rulesHash) }}</template>
      </el-table-column>
      <el-table-column label="生效时刻" width="170">
        <template #default="{ row }">{{ row.seq === 0 ? '自创世生效' : row.effectiveMs === null ? '未生效' : formatDate(row.effectiveMs) }}</template>
      </el-table-column>
    </el-table>
    <template v-if="rulesChain.changes.length > 0">
      <p class="hint">未生效的规则修改条目（归宿由内核按规则修改机制确定性推导）：</p>
      <div v-for="change in rulesChain.changes" :key="change.opHash" class="op-meta">
        <el-tag size="small" :type="change.fate === 'pending' ? 'warning' : 'danger'">
          {{ change.fate === 'pending' ? '待生效' : '已拒绝' }}
        </el-tag>
        <span>{{ shortId(change.opHash) }}</span>
        <span>{{ change.reason }}</span>
      </div>
    </template>
    <p class="hint">现行规则 = 创世规则 + 已生效 rule-change 链（§5.4 每一版本确定性可溯），规则修改只能走集体决策。</p>
  </el-card>
</template>

<script setup lang="ts">
import type { RulesChainView } from './model';
import { formatDate, shortId } from './view-text';

defineProps<{
  rulesChain: RulesChainView;
}>();
</script>

<style scoped>
.hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
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
