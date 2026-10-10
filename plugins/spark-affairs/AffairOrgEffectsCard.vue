<!--
  组织效力卡片（P5 如实口径）：事先声明 × 生效决议的判定与效力应用编排。

  回执口径（诚实边界）：org:effectrcpt: 回执是**本机受理留痕**——内核只按
  affairs:write 桥权限放行，不校验调用方与该组织的成员/管理员关系；其证明力
  为「本机已受理应用」，不是「组织确认生效」凭据。本面板为参考实现演示
  （personal 空间手输 orgId），正式治理操作归组织侧工具。
-->
<template>
  <el-card shadow="never">
    <template #header>
      <h4>组织效力（事先声明 × 生效决议）</h4>
    </template>
    <div class="effects-toolbar">
      <el-input :model-value="orgId" size="small" placeholder="组织 id（org_ 前缀，16/64 hex）" class="effects-org-input"
        @update:model-value="$emit('update:orgId', $event)" />
      <el-button size="small" :loading="loading" @click="$emit('load')">查询效力</el-button>
      <el-button size="small" type="primary" :disabled="!hasPendingEffects" :loading="applying"
        @click="$emit('apply')">应用并写回执</el-button>
    </div>
    <template v-if="orgEffects">
      <el-empty v-if="orgEffects.effects.length === 0" description="该组织对本事务无效力声明/决议" />
      <div v-for="effect in orgEffects.effects" :key="`${effect.scope}:${effect.resolutionOpHash}`" class="op-item">
        <div class="op-meta">
          <el-tag size="small" :type="effect.outcome === 'apply' ? 'success' : 'info'">{{ effectOutcomeText(effect.outcome) }}</el-tag>
          <el-tag v-if="effect.receiptState" size="small" :type="effect.receiptState === 'recorded' ? 'success' : 'warning'">
            {{ effect.receiptState === 'recorded' ? '已应用（本机回执留痕）' : '待应用（未写回执）' }}
          </el-tag>
          <span>scope：{{ effect.scope }}</span>
          <span>决议 {{ shortId(effect.resolutionOpHash) }}</span>
        </div>
      </div>
      <p v-if="orgEffects.invalidResolutions.length > 0" class="hint">
        复算无效决议（不产生效力）：{{ orgEffects.invalidResolutions.map(shortId).join('、') }}
      </p>
    </template>
    <p class="hint">
      名册/策略内容的实际变更不在内核：policy 效力由本插件把决议结果作为策略草稿提交（policy:write，
      发布合入归管理员授权面）；roster/create/budget 效力归组织侧工具——全部应用成功才写回执
      （org:effectrcpt:，幂等、同 scope 取最新决议），未应用不出具「已应用」凭据。
    </p>
    <p class="hint">
      回执口径：org:effectrcpt: 是本机受理留痕——内核只按 affairs:write 桥权限放行，不校验调用方
      与该组织的成员/管理员关系；其证明力为「本机已受理应用」，不是「组织确认生效」凭据。本面板为
      参考实现演示，正式治理操作归组织侧工具。
    </p>
  </el-card>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import type { OrgEffectOutcome, OrgEffectsView } from './model';
import { shortId } from './view-text';

const props = defineProps<{
  orgId: string;
  orgEffects: OrgEffectsView | null;
  loading: boolean;
  applying: boolean;
}>();

defineEmits<{
  (event: 'update:orgId', value: string): void;
  (event: 'load'): void;
  (event: 'apply'): void;
}>();

/** 组织效力判定结果中文文案（org-genesis §6 三线判定） */
function effectOutcomeText(outcome: OrgEffectOutcome): string {
  return {
    apply: '待应用',
    notDeclared: '未事先声明',
    revoked: '声明已撤销',
    resolutionNotEffective: '决议未生效',
    grantNotAnchored: '声明未锚定',
    resolutionNotAnchored: '决议未锚定',
    notPrior: '声明晚于决议（非事先）'
  }[outcome];
}

const hasPendingEffects = computed(() =>
  Boolean(
    props.orgEffects?.effects.some(
      (effect) => effect.outcome === 'apply' && effect.receiptState === 'unrecorded'
    )
  )
);
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
.effects-toolbar {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
  /* 窄窗组织 id 输入框与两个按钮换行堆叠 */
  flex-wrap: wrap;
}
.effects-org-input {
  flex: 1 1 200px;
  min-width: 0;
  max-width: 360px;
}
</style>
