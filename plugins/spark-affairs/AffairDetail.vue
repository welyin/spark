<!--
  公共议题客户端（spark-affairs）· 议题详情：日志时间线、规则、阶梯名册、决议、贡献/评论/投票。

  展示纪律（community-affairs.md §3.3/§3.4 + 已落地 SDK 面）：
  - 时间线按「声明时刻 + opHash」排序仅为展示（model.sortOperations 头注：
    不做跨副本确定性声称；计数/判定一律 opHash 字典序键）；
  - readLog 不暴露逐条操作的存证锚定时刻，时间为签名者声明时刻（自报值），
    关闭/决议判定以内核推导为准（含公示期）；
  - 变更订阅走 sdk.affairs.onChange（AffairChanged 事件驱动重载本事务），
    手动刷新/提交后重载仍保留为兜底（通知非可靠队列）；
  - 操作者身份 id 为协议 actor（本参考实现 = 操作方插件域身份，非个人身份）。

  子组件拆分（coding-standards 500 行建议）：阶梯名册 / 决议 / 规则版本链 /
  执行状态 / 组织效力各自独立卡片组件，共享展示助手见 view-text.ts。

  窗口化适配（ui-architecture §4.2）：元信息 el-descriptions 窄窗单列堆叠
  （ui-layout.isNarrowLayout）；本组件在 el-drawer 内渲染，超高时间线由抽屉
  自带滚动体承载，根节点不设 height/overflow。
-->
<template>
  <div class="affair-detail" v-loading="loading">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <template v-if="detail">
      <el-descriptions :column="isNarrowLayout ? 1 : 2" border size="small" class="rules">
        <el-descriptions-item label="发起人">{{ shortId(detail.originator) }}</el-descriptions-item>
        <el-descriptions-item label="发起时间">{{ formatDate(detail.createdAt) }}（声明值）</el-descriptions-item>
        <el-descriptions-item label="通过阈值">{{ Math.round(detail.rules.passThreshold * 100) }}%</el-descriptions-item>
        <el-descriptions-item label="法定人数">{{ detail.rules.minQuorum }}</el-descriptions-item>
        <el-descriptions-item label="公示期">{{ detail.rules.reviewPeriodHours }} 小时</el-descriptions-item>
        <el-descriptions-item label="参与门槛">{{ entryRequirementText }}</el-descriptions-item>
        <el-descriptions-item label="状态">
          <el-tag :type="detail.closed ? 'info' : 'success'">{{ detail.closed ? '已关闭' : '进行中' }}</el-tag>
        </el-descriptions-item>
        <el-descriptions-item label="初始投票者">{{ detail.rules.initialVoters.length || '仅发起人' }}</el-descriptions-item>
        <el-descriptions-item v-if="detail.refs.length > 0" label="引用（§10）">
          <el-tag v-for="refItem in detail.refs" :key="`${refItem.rel}:${refItem.target}`" size="small" type="info" class="ref-tag">
            {{ relText(refItem.rel) }} {{ shortId(refItem.target) }}
          </el-tag>
        </el-descriptions-item>
      </el-descriptions>

      <AffairLadderCard :roster="roster" :my-ladder="myLadder" />

      <el-card shadow="never" class="composer-card">
        <el-tabs>
          <el-tab-pane label="提交贡献">
            <p class="hint" v-if="myLadder && !canSubmitContribution(myLadder.level)">
              贡献者及以上才能提交正式贡献；当前为观察者，可先评论参与讨论（观察层永远零门槛）。
            </p>
            <el-input v-model="contributionText" type="textarea" :rows="3" maxlength="2000" show-word-limit
              placeholder="议案文本 / PR 链接 / 数据……采纳走「延迟生效+阈值否决」集体决策" />
            <div class="actions">
              <el-button type="primary" :disabled="myLadder ? !canSubmitContribution(myLadder.level) : false"
                :loading="submitting" @click="submitContribution">提交贡献</el-button>
            </div>
          </el-tab-pane>
          <el-tab-pane label="评论">
            <el-input v-model="commentText" type="textarea" :rows="2" maxlength="500" show-word-limit
              placeholder="参与讨论（零门槛）" />
            <div class="actions">
              <el-button type="primary" :loading="submitting" @click="submitComment">发表评论</el-button>
            </div>
          </el-tab-pane>
        </el-tabs>
        <p class="hint">
          提交将请求一次签名：操作以本插件域身份作为协议 actor 签名（affairs:write），
          内核入站校验链全过才入日志；签名不证明操作者的个人身份。
        </p>
      </el-card>

      <el-card shadow="never">
        <template #header>
          <div class="header-row">
            <h4>操作日志</h4>
            <span>
              {{ operations.length }} 条
              <el-button size="small" text @click="reload">刷新</el-button>
            </span>
          </div>
        </template>
        <el-empty v-if="operations.length === 0" description="暂无操作" />
        <div v-for="op in operations" :key="op.opHash" class="op-item">
          <div class="op-meta">
            <el-tag size="small" :type="opKindTagType(op.kind)">{{ opKindText(op.kind) }}</el-tag>
            <strong>{{ shortId(op.author) }}</strong>
            <span>{{ formatDate(op.declaredAt) }}（声明时刻）</span>
            <el-tag v-if="op.kind === 'vote'" size="small">{{ voteChoiceText(op.payload as VotePayload) }}</el-tag>
          </div>
          <p class="op-content">{{ operationText(op) }}</p>
          <div class="op-actions" v-if="op.kind === 'contribution'">
            <el-button size="small" :disabled="!canVoteNow" :loading="votingHash === op.opHash"
              @click="vote(op.opHash, 'for')">赞成</el-button>
            <el-button size="small" type="danger" plain :disabled="!canVoteNow" :loading="votingHash === op.opHash"
              @click="vote(op.opHash, 'against')">反对</el-button>
            <el-button size="small" plain :disabled="!canVoteNow" :loading="votingHash === op.opHash"
              @click="vote(op.opHash, 'abstain')">弃权</el-button>
          </div>
          <p class="hint" v-if="op.kind === 'vote' && (op.payload as VotePayload).identityMode === 'public'">
            历史票载荷标注「公共身份」：仅为自报标签，无密码学效力——协议 actor 为插件域身份，
            平台无个人身份签名面，不据此计入任何公开履历。
          </p>
        </div>
      </el-card>

      <AffairResolutionCard
        :resolutions="resolutions"
        :resolution-plan="resolutionPlan"
        :closed="detail.closed"
        :min-quorum="detail.rules.minQuorum"
        :resolving="resolving"
        @submit="submitResolution"
      />

      <AffairRulesChainCard v-if="rulesChain" :rules-chain="rulesChain" />

      <AffairExecStatesCard v-if="execStates.length > 0" :exec-states="execStates" />

      <AffairOrgEffectsCard
        v-model:org-id="effectsOrgId"
        :org-effects="orgEffects"
        :loading="effectsLoading"
        :applying="effectsApplying"
        @load="loadOrgEffects"
        @apply="applyOrgEffects"
      />
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import type { AffairRefRel } from '../../packages/plugin-sdk/src/affair-wire';
import AffairExecStatesCard from './AffairExecStatesCard.vue';
import AffairLadderCard from './AffairLadderCard.vue';
import AffairOrgEffectsCard from './AffairOrgEffectsCard.vue';
import AffairResolutionCard from './AffairResolutionCard.vue';
import AffairRulesChainCard from './AffairRulesChainCard.vue';
import type { AffairsService } from './service';
import { isNarrowLayout } from './ui-layout';
import { formatDate, levelText, shortId } from './view-text';
import {
  canSubmitContribution,
  canVote,
  type AffairDetail,
  type AffairOperation,
  type AffairResolutionView,
  type CommentPayload,
  type ContributionPayload,
  type ExecStateView,
  type LadderEntryView,
  type LadderState,
  type OrgEffectsView,
  type ResolutionPlan,
  type RulesChainView,
  type VotePayload
} from './model';

const props = defineProps<{
  service: AffairsService;
  affairId: string;
}>();

const loading = ref(false);
const message = ref('');
const messageType = ref<'success' | 'error'>('success');
const detail = ref<AffairDetail | null>(null);
const operations = ref<AffairOperation[]>([]);
const roster = ref<{ entries: LadderEntryView[]; voters: string[] } | null>(null);
const myLadder = ref<LadderState | null>(null);
const resolutions = ref<AffairResolutionView[]>([]);
/** 决议计划（关闭条件满足判定 + 计票预览；null = 暂不能发起决议） */
const resolutionPlan = ref<ResolutionPlan | null>(null);
const resolving = ref(false);
const rulesChain = ref<RulesChainView | null>(null);
const execStates = ref<ExecStateView[]>([]);
const effectsOrgId = ref('');
const orgEffects = ref<OrgEffectsView | null>(null);
const effectsLoading = ref(false);
const effectsApplying = ref(false);
const contributionText = ref('');
const commentText = ref('');
const submitting = ref(false);
const votingHash = ref<string | null>(null);

// 已知本机身份（有写操作历史）时按名册门槛收敛按钮；否则放开、资格以内核推导为准
const canVoteNow = computed(() => (myLadder.value ? canVote(myLadder.value.level) : true));
const entryRequirementText = computed(() => {
  if (!detail.value) {
    return '-';
  }
  const requirement = detail.value.rules.entryRequirement;
  if (requirement.kind === 'none') {
    return '零门槛（观察/评论）';
  }
  if (requirement.kind === 'ladder') {
    return `阶梯 ≥ ${levelText(requirement.minLevel)}`;
  }
  return `凭证：${requirement.credentialType}`;
});

function show(text: string, type: 'success' | 'error'): void {
  message.value = text;
  messageType.value = type;
}

/** 引用关系中文文案（affair.md §10 枚举） */
function relText(rel: AffairRefRel): string {
  return { inherit: '继承', appeal: '申诉', parent: '父子', related: '关联' }[rel];
}

function opKindText(kind: AffairOperation['kind']): string {
  return { contribution: '贡献', vote: '投票', comment: '评论' }[kind];
}

function opKindTagType(kind: AffairOperation['kind']): 'primary' | 'success' | 'info' {
  return ({ contribution: 'primary', vote: 'success', comment: 'info' } as const)[kind];
}

function voteChoiceText(payload: VotePayload): string {
  return { for: '赞成', against: '反对', abstain: '弃权' }[payload.choice];
}

function operationText(op: AffairOperation): string {
  if (op.kind === 'vote') {
    return `对操作 ${(op.payload as VotePayload).targetOpHash.slice(0, 12)}… 投票`;
  }
  return (op.payload as ContributionPayload | CommentPayload).text;
}

async function reload(): Promise<void> {
  loading.value = true;
  try {
    const [nextDetail, nextOps, nextRoster, nextMine, nextResolutions, nextRulesChain, nextExecStates, nextPlan] = await Promise.all([
      props.service.getDetail(props.affairId),
      props.service.listOperations(props.affairId),
      props.service.getLadderRoster(props.affairId),
      props.service.getMyLadderState(props.affairId),
      props.service.listResolutions(props.affairId),
      props.service.getRulesChain(props.affairId),
      props.service.getExecStates(props.affairId),
      props.service.getResolutionPlan(props.affairId)
    ]);
    detail.value = nextDetail;
    operations.value = nextOps;
    roster.value = nextRoster;
    myLadder.value = nextMine;
    resolutions.value = nextResolutions;
    rulesChain.value = nextRulesChain;
    execStates.value = nextExecStates;
    resolutionPlan.value = nextPlan;
  } catch (error) {
    show(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
  }
}

/** 发起决议（结案）：构造 resolution 操作签名提交，内核逐副本复算把关有效性 */
async function submitResolution(): Promise<void> {
  resolving.value = true;
  try {
    await props.service.submitResolution(props.affairId);
    show('决议已提交：进入公示期，期内无阈值异议即生效；复算不符将如实标为无效决议。', 'success');
    await reload();
  } catch (error) {
    show(`发起决议失败：${(error as Error).message}`, 'error');
  } finally {
    resolving.value = false;
  }
}

/** 查询组织效力（orgEffects：逐声明 × 逐决议判定 + 回执状态标注） */
async function loadOrgEffects(): Promise<void> {
  const orgId = effectsOrgId.value.trim();
  if (!orgId) {
    show('请先填写组织 id（org_ 前缀）。', 'error');
    return;
  }
  effectsLoading.value = true;
  try {
    orgEffects.value = await props.service.getOrgEffects(orgId, props.affairId);
    if (!orgEffects.value) {
      show('效力查询返回形状不符。', 'error');
    }
  } catch (error) {
    show(`效力查询失败：${(error as Error).message}`, 'error');
  } finally {
    effectsLoading.value = false;
  }
}

/** 效力应用编排：内容应用全部成功才写回执（fail-closed），完成后重读收敛 */
async function applyOrgEffects(): Promise<void> {
  const orgId = effectsOrgId.value.trim();
  if (!orgId) {
    return;
  }
  effectsApplying.value = true;
  try {
    const report = await props.service.applyOrgEffects(orgId, props.affairId);
    if (report.receiptActions) {
      const recorded = report.receiptActions.filter((action) => action.action !== 'skipped');
      show(`效力已应用并写本机回执（${recorded.length} 条）：${recorded.map((action) => `${action.scope}=${action.action}`).join('、')}`, 'success');
    } else if (report.unapplied.length > 0) {
      show(
        `部分效力未能应用，未写回执：${report.unapplied.map((item) => `${item.scope}（${item.reason}）`).join('；')}`,
        'error'
      );
    } else {
      show('没有待应用的效力事件（均已回执或无 Apply 判定）。', 'success');
    }
    await loadOrgEffects();
  } catch (error) {
    show(`效力应用失败：${(error as Error).message}`, 'error');
  } finally {
    effectsApplying.value = false;
  }
}

async function submitContribution(): Promise<void> {
  submitting.value = true;
  try {
    await props.service.submitContribution(props.affairId, contributionText.value);
    contributionText.value = '';
    show('贡献已提交，进入「延迟生效+阈值否决」公示。', 'success');
    await reload();
  } catch (error) {
    show(`提交失败：${(error as Error).message}`, 'error');
  } finally {
    submitting.value = false;
  }
}

async function submitComment(): Promise<void> {
  submitting.value = true;
  try {
    await props.service.submitComment(props.affairId, commentText.value);
    commentText.value = '';
    show('评论已发表。', 'success');
    await reload();
  } catch (error) {
    show(`发表失败：${(error as Error).message}`, 'error');
  } finally {
    submitting.value = false;
  }
}

async function vote(targetOpHash: string, choice: 'for' | 'against' | 'abstain'): Promise<void> {
  votingHash.value = targetOpHash;
  try {
    await props.service.submitVote(props.affairId, targetOpHash, choice);
    show('投票已记录（一人一票，不加权）。', 'success');
    await reload();
  } catch (error) {
    show(`投票失败：${(error as Error).message}`, 'error');
  } finally {
    votingHash.value = null;
  }
}

onMounted(async () => {
  // 变更订阅（sdk.affairs.onChange）：只关心本事务的事件，收到即重读收敛；
  // 订阅失败降级为手动刷新（onChange 无退订面，组件随抽屉关闭销毁，handler 随之失效）
  try {
    await props.service.subscribeChanges((event) => {
      if (event.affairId === props.affairId) {
        void reload();
      }
    });
  } catch (error) {
    console.warn('[spark-affairs] 详情变更订阅不可用，降级为手动刷新：', error);
  }
  await reload();
});
</script>

<style scoped>
.affair-detail {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.message {
  margin-bottom: 4px;
}
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  /* 窄窗标题与操作区（计数 + 刷新）换行堆叠 */
  flex-wrap: wrap;
  row-gap: 4px;
}
.composer-card h4 {
  margin: 0;
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
  /* 窄窗多标签 + 长身份 id 换行堆叠 */
  flex-wrap: wrap;
}
.op-content {
  margin: 6px 0;
  font-size: 13px;
  white-space: pre-wrap;
}
.op-actions {
  display: flex;
  gap: 8px;
  /* 窄窗赞成/反对/弃权三钮换行 */
  flex-wrap: wrap;
}
.ref-tag {
  margin-right: 6px;
}
</style>
