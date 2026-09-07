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
-->
<template>
  <div class="affair-detail" v-loading="loading">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <template v-if="detail">
      <el-descriptions :column="2" border size="small" class="rules">
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
              <el-tooltip content="上下文身份（默认）：跨事务不可关联；公共身份：参与计入公开履历，启用即明示永久公开">
                <el-switch v-model="identityModeIsPublic" active-text="公共身份" inactive-text="上下文身份" />
              </el-tooltip>
              · {{ operations.length }} 条
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
          <p class="hint" v-if="op.kind === 'vote'">票权身份：{{ (op.payload as VotePayload).identityMode === 'public' ? '公共身份（计入公开履历）' : '上下文身份（默认，跨事务不可关联）' }}</p>
        </div>
      </el-card>

      <el-card v-if="resolutions.length > 0" shadow="never">
        <template #header>
          <h4>决议（{{ resolutions.length }}）</h4>
        </template>
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

      <el-card v-if="rulesChain" shadow="never">
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

      <el-card v-if="execStates.length > 0" shadow="never">
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

      <el-card shadow="never">
        <template #header>
          <h4>组织效力（事先声明 × 生效决议）</h4>
        </template>
        <div class="effects-toolbar">
          <el-input v-model="effectsOrgId" size="small" placeholder="组织 id（org_ 前缀，16/64 hex）" class="effects-org-input" />
          <el-button size="small" :loading="effectsLoading" @click="loadOrgEffects">查询效力</el-button>
          <el-button size="small" type="primary" :disabled="!hasPendingEffects" :loading="effectsApplying"
            @click="applyOrgEffects">应用并写回执</el-button>
        </div>
        <template v-if="orgEffects">
          <el-empty v-if="orgEffects.effects.length === 0" description="该组织对本事务无效力声明/决议" />
          <div v-for="effect in orgEffects.effects" :key="`${effect.scope}:${effect.resolutionOpHash}`" class="op-item">
            <div class="op-meta">
              <el-tag size="small" :type="effect.outcome === 'apply' ? 'success' : 'info'">{{ effectOutcomeText(effect.outcome) }}</el-tag>
              <el-tag v-if="effect.receiptState" size="small" :type="effect.receiptState === 'recorded' ? 'success' : 'warning'">
                {{ effect.receiptState === 'recorded' ? '已生效（回执已落库）' : '待应用（未写回执）' }}
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
          （org:effectrcpt:，幂等、同 scope 取最新决议），未应用不出具「已生效」凭据。
        </p>
      </el-card>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import type { AffairRefRel } from '../../packages/plugin-sdk/src/affair-wire';
import type { AffairsService } from './service';
import {
  canSubmitContribution,
  canVote,
  type AffairDetail,
  type AffairOperation,
  type AffairResolutionState,
  type AffairResolutionView,
  type CommentPayload,
  type ContributionPayload,
  type ExecStateName,
  type ExecStateView,
  type LadderEntryView,
  type LadderLevel,
  type LadderState,
  type OrgEffectOutcome,
  type OrgEffectsView,
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
const identityMode = ref<'contextual' | 'public'>('contextual');
const identityModeIsPublic = computed({
  get: () => identityMode.value === 'public',
  set: (value: boolean) => {
    identityMode.value = value ? 'public' : 'contextual';
  }
});

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
const myLadderText = computed(() => {
  if (!myLadder.value) {
    return '-';
  }
  return `${levelText(myLadder.value.level)}（账龄 ${myLadder.value.accountAgeDays} 天）`;
});
const ladderTagType = computed<'info' | 'warning' | 'success'>(() => {
  if (!myLadder.value) {
    return 'info';
  }
  return myLadder.value.level === 'voter' ? 'success' : myLadder.value.level === 'contributor' ? 'warning' : 'info';
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

function levelText(level: LadderLevel): string {
  return { observer: '观察者', contributor: '贡献者', voter: '投票者' }[level];
}

/** 引用关系中文文案（affair.md §10 枚举） */
function relText(rel: AffairRefRel): string {
  return { inherit: '继承', appeal: '申诉', parent: '父子', related: '关联' }[rel];
}

function opKindText(kind: AffairOperation['kind']): string {
  return { contribution: '贡献', vote: '投票', comment: '评论' }[kind];
}

function opKindTagType(kind: AffairOperation['kind']): 'primary' | 'success' | 'info' {
  return { contribution: 'primary', vote: 'success', comment: 'info' }[kind];
}

function voteChoiceText(payload: VotePayload): string {
  return { for: '赞成', against: '反对', abstain: '弃权' }[payload.choice];
}

function resolutionStateText(state: AffairResolutionState): string {
  return { pending: '公示中', effective: '已生效', vetoed: '已否决', unanchored: '未锚定' }[state];
}

function resolutionTagType(state: AffairResolutionState): 'info' | 'success' | 'danger' | 'warning' {
  return { pending: 'warning', effective: 'success', vetoed: 'danger', unanchored: 'info' }[state];
}

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
  return {
    unanchored: 'info',
    'resolution-pending': 'warning',
    'resolution-vetoed': 'danger',
    'awaiting-execution': 'warning',
    'in-progress': 'primary',
    verifying: 'warning',
    returned: 'danger',
    closed: 'success'
  }[state];
}

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
    orgEffects.value?.effects.some(
      (effect) => effect.outcome === 'apply' && effect.receiptState === 'unrecorded'
    )
  )
);

function operationText(op: AffairOperation): string {
  if (op.kind === 'vote') {
    return `对操作 ${(op.payload as VotePayload).targetOpHash.slice(0, 12)}… 投票`;
  }
  return (op.payload as ContributionPayload | CommentPayload).text;
}

async function reload(): Promise<void> {
  loading.value = true;
  try {
    const [nextDetail, nextOps, nextRoster, nextMine, nextResolutions, nextRulesChain, nextExecStates] = await Promise.all([
      props.service.getDetail(props.affairId),
      props.service.listOperations(props.affairId),
      props.service.getLadderRoster(props.affairId),
      props.service.getMyLadderState(props.affairId),
      props.service.listResolutions(props.affairId),
      props.service.getRulesChain(props.affairId),
      props.service.getExecStates(props.affairId)
    ]);
    detail.value = nextDetail;
    operations.value = nextOps;
    roster.value = nextRoster;
    myLadder.value = nextMine;
    resolutions.value = nextResolutions;
    rulesChain.value = nextRulesChain;
    execStates.value = nextExecStates;
  } catch (error) {
    show(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
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
      show(`效力已应用并写回执（${recorded.length} 条）：${recorded.map((action) => `${action.scope}=${action.action}`).join('、')}`, 'success');
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
    await props.service.submitVote(props.affairId, targetOpHash, choice, identityMode.value);
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
}
.ladder-card h4,
.composer-card h4 {
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
}
.op-content {
  margin: 6px 0;
  font-size: 13px;
  white-space: pre-wrap;
}
.op-actions {
  display: flex;
  gap: 8px;
}
.ref-tag {
  margin-right: 6px;
}
.effects-toolbar {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
}
.effects-org-input {
  max-width: 360px;
}
</style>
