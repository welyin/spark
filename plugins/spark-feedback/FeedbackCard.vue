<!--
  问题反馈应用（spark-feedback）· 反馈回执卡片（message-card 视图）。

  「消息自描述」约定（同 spark-git-repo/PrCard.vue）：card.data 自带类型/标题/
  子事务 id 摘要字段，卡片不依赖事务读取即可呈现；「查看子事务」经
  triggerCardAction 上行给主视图，由主视图 sdk.navigation 深链打开事务界面。
-->
<template>
  <section class="feedback-card">
    <div class="card-head">
      <span class="badge" :data-type="type">{{ typeLabel }}</span>
      <strong class="title">{{ title }}</strong>
    </div>
    <p class="meta">已回流为目标项目议题的子事务（{{ shortId }}…），处理进展在「问题反馈 · 我的反馈」中查看。</p>
    <div class="actions">
      <button class="link-btn" @click="openAffair">查看子事务</button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{
  cardData?: { type?: string; title?: string; childAffairId?: string; targetAffairId?: string };
}>();

const type = computed(() => props.cardData?.type ?? 'bug');
const typeLabel = computed(() => (type.value === 'proposal' ? '建议' : '缺陷'));
const title = computed(() => props.cardData?.title ?? '(反馈)');
const shortId = computed(() => props.cardData?.childAffairId?.slice(0, 12) ?? '');

function openAffair(): void {
  const sdk = window.__sparkPluginSDK;
  const affairId = props.cardData?.childAffairId;
  if (!sdk?.messages || !affairId) {
    return;
  }
  sdk.messages.triggerCardAction('open-affair', { affairId });
}
</script>

<style scoped>
.feedback-card {
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
  background: #fff7e6;
  color: #d46b08;
}
.badge[data-type='proposal'] {
  background: #e6f4ff;
  color: #1677ff;
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
.actions {
  margin-top: 6px;
}
.link-btn {
  border: none;
  background: none;
  padding: 0;
  color: #1677ff;
  cursor: pointer;
  font-size: 13px;
}
</style>
