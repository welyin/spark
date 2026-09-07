<!--
  公共议题客户端（spark-affairs）· 主视图：议题墙（本机关注）+ 发起议题 + 按创世记录关注。

  结构（.vue 行数约束，按区块拆子组件）：
  - 本文件：壳层（SDK 可用性降级、发起表单、按创世记录关注、议题墙列表、详情抽屉开关）；
  - AffairDetail.vue：议题详情（日志时间线、规则、阶梯名册、贡献/评论/投票）；
  - AffairCard.vue + affair-card.ts：message-card 卡片视图（独立 bundle）。

  诚实口径：已落地 sdk.affairs 没有 indexer 目录面（listWall 不存在），
  议题墙 = 本机关注的议题（关注即持有副本）；关注已有议题需粘贴其创世记录
  原文（affairId 由创世自认证复算，不接受自报 id）。
-->
<template>
  <section class="spark-affairs">
    <el-alert v-if="unavailable" type="warning" :closable="false" show-icon class="message"
      title="当前宿主未提供可用的 sdk.affairs（共同体事务 SDK 模块），本参考插件暂不可用。" />

    <template v-else>
      <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

      <el-card shadow="never" class="header-card">
        <div class="header-row">
          <div>
            <p class="eyebrow">公共事务</p>
            <h2>议题墙</h2>
            <p class="lede">
              事务全局一等：只有发起人、没有归属，关注即持有一份副本。观察/评论永远零门槛。
              本墙列出本机关注的议题；发起人与操作者身份为插件域身份 id（平台暂无个人身份签名面，见详情页说明）。
            </p>
          </div>
          <el-button @click="reload" :loading="loading">刷新</el-button>
        </div>
      </el-card>

      <el-card shadow="never" class="composer-card">
        <template #header>
          <h3>发起议题</h3>
        </template>
        <el-form label-position="top">
          <el-form-item label="标题">
            <el-input v-model="draft.title" maxlength="80" show-word-limit placeholder="议题标题" />
          </el-form-item>
          <el-form-item label="简介">
            <el-input v-model="draft.summary" type="textarea" :rows="3" maxlength="500" show-word-limit
              placeholder="议题简介（同时作为首条开题说明入操作日志，修订走集体决策）" />
          </el-form-item>
          <el-form-item label="标签（空格分隔）">
            <el-input v-model="draft.tagsText" placeholder="如：开源 预算 业委会" />
          </el-form-item>
          <el-form-item label="引用其他事务（可选；append-only 不可撤销，自指由内核拒绝）">
            <div v-for="(item, index) in draft.refs" :key="index" class="ref-row">
              <el-select v-model="item.rel" class="ref-rel">
                <el-option v-for="opt in REL_OPTIONS" :key="opt.value" :label="opt.label" :value="opt.value" />
              </el-select>
              <el-input v-model="item.target" placeholder="目标事务 affairId（64 位小写 hex）" />
              <el-button size="small" text type="danger" @click="draft.refs.splice(index, 1)">移除</el-button>
            </div>
            <el-button size="small" text @click="draft.refs.push({ target: '', rel: 'related' })">+ 添加引用</el-button>
          </el-form-item>
          <el-form-item label="关闭规则（创世规则，之后只能走集体决策修改）">
            <div class="rules-grid">
              <label>
                通过阈值
                <el-slider v-model="draft.passThreshold" :min="0.5" :max="1" :step="0.05" show-input />
              </label>
              <label>
                法定人数
                <el-input-number v-model="draft.minQuorum" :min="1" :max="100000" />
              </label>
              <label>
                公示期（小时，协议下限 24）
                <el-input-number v-model="draft.reviewPeriodHours" :min="24" :max="720" />
              </label>
              <label>
                参与门槛
                <el-select v-model="draft.entryKind">
                  <el-option label="零门槛（观察/评论）" value="none" />
                  <el-option label="贡献者及以上" value="contributor" />
                  <el-option label="仅投票者" value="voter" />
                </el-select>
              </label>
            </div>
          </el-form-item>
          <p class="hint">发起 = 本地构造创世记录并以本插件域身份签名（affairs:write），内核全链校验后 affairId 自认证复算。</p>
          <div class="actions">
            <el-button type="primary" :loading="creating" @click="submitCreate">发起议题</el-button>
          </div>
        </el-form>
      </el-card>

      <el-card shadow="never">
        <template #header>
          <h3>关注已有议题</h3>
        </template>
        <el-input v-model="genesisText" type="textarea" :rows="3"
          placeholder="粘贴议题的创世记录 JSON（affairId 由创世记录自认证复算，不接受自报 id）" />
        <div class="actions">
          <el-button :loading="following" @click="followByGenesis">校验并关注</el-button>
        </div>
      </el-card>

      <el-card shadow="never">
        <template #header>
          <h3>我关注的议题</h3>
        </template>

        <el-empty v-if="affairs.length === 0" description="暂无关注的议题" />
        <div v-for="affair in affairs" :key="affair.affairId" class="affair-item">
          <div class="affair-meta">
            <strong>{{ affair.title }}</strong>
            <el-tag v-for="tag in affair.tags" :key="tag" size="small" type="info">{{ tag }}</el-tag>
          </div>
          <p class="affair-summary">{{ affair.summary }}</p>
          <div class="affair-actions">
            <span class="affair-origin">发起人 {{ shortId(affair.originator) }} · {{ formatDate(affair.createdAt) }} ·
              {{ affair.operationCount }} 条操作</span>
            <span>
              <el-button size="small" @click="openDetail(affair.affairId)">打开</el-button>
              <el-button size="small" type="warning" :loading="togglingId === affair.affairId"
                @click="unfollow(affair)">取关</el-button>
            </span>
          </div>
        </div>
      </el-card>

      <el-drawer v-model="detailOpen" size="70%" :title="detailTitle">
        <AffairDetail v-if="detailId" :service="serviceRef" :affair-id="detailId" />
      </el-drawer>
    </template>
  </section>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue';
import { ensurePluginSDK, type PluginSDK } from '../../packages/plugin-sdk/src';
import type { AffairRef, AffairRefRel } from '../../packages/plugin-sdk/src/affair-wire';
import { AffairsService } from './service';
import { AFFAIR_TAG_MAX_COUNT, type AffairCreateInput, type AffairListItem } from './model';
import AffairDetail from './AffairDetail.vue';

/** 事务间引用关系下拉（affair.md §10 枚举；文案为产品解释，线形只带 rel 字面量） */
const REL_OPTIONS: Array<{ value: AffairRefRel; label: string }> = [
  { value: 'inherit', label: '继承 inherit（承接目标事务）' },
  { value: 'appeal', label: '申诉 appeal（对目标事务结论申诉）' },
  { value: 'parent', label: '父子 parent（从属于目标事务）' },
  { value: 'related', label: '关联 related（弱引用）' }
];

const unavailable = ref(false);
const message = ref('');
const messageType = ref<'success' | 'error'>('success');
const loading = ref(false);
const creating = ref(false);
const following = ref(false);
const togglingId = ref<string | null>(null);
const affairs = ref<AffairListItem[]>([]);
const genesisText = ref('');
const detailOpen = ref(false);
const detailId = ref<string | null>(null);
const detailTitle = ref('议题详情');

let sdk: PluginSDK;
let service: AffairsService;
// 详情子组件经 prop 取用（template 中对 ref 自动解包）
const serviceRef = ref<AffairsService | null>(null);

const draft = reactive({
  title: '',
  summary: '',
  tagsText: '',
  refs: [] as AffairRef[],
  passThreshold: 0.67,
  minQuorum: 1,
  reviewPeriodHours: 24,
  entryKind: 'none' as 'none' | 'contributor' | 'voter'
});

function show(text: string, type: 'success' | 'error'): void {
  message.value = text;
  messageType.value = type;
}

function formatDate(ts: number): string {
  return new Date(ts).toLocaleString();
}

function shortId(identity: string): string {
  return identity.length > 16 ? `${identity.slice(0, 12)}…` : identity;
}

async function reload(): Promise<void> {
  loading.value = true;
  try {
    affairs.value = await service.listFollowed();
  } catch (error) {
    show(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
  }
}

async function submitCreate(): Promise<void> {
  creating.value = true;
  try {
    const input: AffairCreateInput = {
      title: draft.title,
      summary: draft.summary,
      tags: draft.tagsText.split(/\s+/).filter(Boolean).slice(0, AFFAIR_TAG_MAX_COUNT),
      // 空行视为未填；形状校验（rel 枚举 + 64 hex）在 validateAffairDraft
      refs: draft.refs
        .map((item) => ({ target: item.target.trim(), rel: item.rel }))
        .filter((item) => item.target.length > 0),
      rules: {
        reviewPeriodHours: draft.reviewPeriodHours,
        passThreshold: draft.passThreshold,
        minQuorum: draft.minQuorum,
        initialVoters: [],
        entryRequirement:
          draft.entryKind === 'none'
            ? { kind: 'none' }
            : { kind: 'ladder', minLevel: draft.entryKind }
      }
    };
    const { affairId } = await service.createAffair(input);
    const notified = await service.notifyNewAffair(input.title, affairId);
    show(notified ? '议题已发起，本机应用会话已收到通知。' : '议题已发起。', 'success');
    draft.title = '';
    draft.summary = '';
    draft.tagsText = '';
    draft.refs = [];
    await reload();
  } catch (error) {
    show(`发起失败：${(error as Error).message}`, 'error');
  } finally {
    creating.value = false;
  }
}

async function followByGenesis(): Promise<void> {
  following.value = true;
  try {
    let genesis: unknown;
    try {
      genesis = JSON.parse(genesisText.value);
    } catch {
      throw new Error('创世记录不是合法 JSON');
    }
    // 关注即持有副本：只要还有副本存在，事务就不会消失
    const affairId = await service.followGenesis(genesis);
    show(`已关注议题 ${affairId.slice(0, 12)}…，本机持有副本。`, 'success');
    genesisText.value = '';
    await reload();
  } catch (error) {
    show(`关注失败：${(error as Error).message}`, 'error');
  } finally {
    following.value = false;
  }
}

async function unfollow(affair: AffairListItem): Promise<void> {
  togglingId.value = affair.affairId;
  try {
    await service.unfollow(affair.affairId);
    show('已取关；已复制的数据保留，无人持有的副本自然消亡。', 'success');
    await reload();
  } catch (error) {
    show(`操作失败：${(error as Error).message}`, 'error');
  } finally {
    togglingId.value = null;
  }
}

async function openDetail(affairId: string): Promise<void> {
  detailId.value = affairId;
  detailTitle.value = affairs.value.find((item) => item.affairId === affairId)?.title ?? '议题详情';
  detailOpen.value = true;
}

onMounted(async () => {
  try {
    sdk = await ensurePluginSDK();
  } catch {
    unavailable.value = true;
    return;
  }
  if (!AffairsService.isAvailable(sdk)) {
    unavailable.value = true;
    return;
  }
  service = new AffairsService(sdk);
  serviceRef.value = service;
  // 变更订阅（sdk.affairs.onChange）：关注/取关/提交/复制合入后事件驱动刷新——
  // 替代纯手动刷新兜底。通知非可靠队列，收到即整单重读收敛；订阅失败降级手动刷新。
  service
    .subscribeChanges(() => {
      void reload();
    })
    .catch((error) => console.warn('[spark-affairs] 变更订阅不可用，降级为手动刷新：', error));
  await reload();
});
</script>

<style scoped>
.spark-affairs {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.message {
  margin-bottom: 4px;
}
.header-card h2 {
  margin: 0 0 4px;
}
.eyebrow {
  margin: 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.lede {
  margin: 0;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 8px;
}
.composer-card .actions {
  display: flex;
  justify-content: flex-end;
}
.rules-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
  width: 100%;
}
.rules-grid label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
  color: var(--el-text-color-regular);
}
.ref-row {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
  width: 100%;
}
.ref-rel {
  max-width: 240px;
  flex-shrink: 0;
}
.hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 8px;
}
.affair-item {
  padding: 10px 0;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.affair-item:last-child {
  border-bottom: none;
}
.affair-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.affair-summary {
  margin: 6px 0;
  font-size: 13px;
  color: var(--el-text-color-regular);
}
.affair-actions {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.affair-origin {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
</style>
