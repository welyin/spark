<!--
  阶梯名册卡片：名册条目、我的阶梯状态。
  名册由内核从日志与存证链确定性推导（ladderStatus），插件只呈现。
-->
<template>
  <el-card shadow="never" class="ladder-card">
    <template #header>
      <div class="header-row">
        <h4>阶梯名册（{{ roster?.entries.length ?? 0 }} 人 · 投票者 {{ roster?.voters.length ?? 0 }} 人）</h4>
        <el-tag v-if="myLadder" :type="ladderTagType">{{ myLadderText }}</el-tag>
      </div>
    </template>
    <p class="hint" v-if="myLadder">
      我的状态：账龄 {{ myLadder.accountAgeDays }} 天 · 被采纳贡献 {{ myLadder.adoptedContributions }} 次 ·
      在级 {{ myLadder.daysAtCurrentLevel }} 天 · 最近活跃 {{ myLadder.lastActiveDaysAgo }} 天前
      <template v-if="myLadder.decayWarning">（长期无活动将降回贡献者）</template>
    </p>
    <el-table v-if="roster && roster.entries.length > 0" :data="roster.entries" size="small" class="roster">
      <el-table-column label="身份 id">
        <template #default="{ row }">{{ shortId(row.identity) }}</template>
      </el-table-column>
      <el-table-column label="级别" width="90">
        <template #default="{ row }">{{ levelText(row.level) }}</template>
      </el-table-column>
      <el-table-column label="采纳" prop="adoptedContributions" width="70" />
      <el-table-column label="账龄（天）" width="90">
        <template #default="{ row }">{{ row.accountAgeDays ?? '—' }}</template>
      </el-table-column>
      <el-table-column label="最近活跃" width="110">
        <template #default="{ row }">{{ row.lastActiveDaysAgo === null ? '—' : `${row.lastActiveDaysAgo} 天前` }}</template>
      </el-table-column>
    </el-table>
    <p class="hint">阶梯回答「你做过什么」，凭证回答「你是谁」——资格判定由事务规则声明，内核从日志与存证链确定性推导，插件伪造不了。</p>
  </el-card>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import type { LadderEntryView, LadderState } from './model';
import { levelText, shortId } from './view-text';

const props = defineProps<{
  roster: { entries: LadderEntryView[]; voters: string[] } | null;
  myLadder: LadderState | null;
}>();

const myLadderText = computed(() => {
  if (!props.myLadder) {
    return '-';
  }
  return `${levelText(props.myLadder.level)}（账龄 ${props.myLadder.accountAgeDays} 天）`;
});

const ladderTagType = computed<'info' | 'warning' | 'success'>(() => {
  if (!props.myLadder) {
    return 'info';
  }
  return props.myLadder.level === 'voter' ? 'success' : props.myLadder.level === 'contributor' ? 'warning' : 'info';
});
</script>

<style scoped>
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  row-gap: 4px;
}
.ladder-card h4 {
  margin: 0;
}
.hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.roster {
  margin-top: 8px;
}
</style>
