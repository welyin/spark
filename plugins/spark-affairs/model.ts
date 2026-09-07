/**
 * 公共议题客户端（spark-affairs）· 数据模型与纯函数。
 *
 * 语义对齐：wiki/product/community-model.md §六（事务容器规则、参与阶梯）、
 * wiki/architecture/community-affairs.md §3（诚实边界：到达顺序不可依赖，
 * 关闭条件只能依赖操作集合内容；时间条件以存证链时间戳为准 + 公示期吸收）。
 * 协议线形（创世/操作/canonical/哈希）在 wire.ts，本文件只承载客户端视图
 * 模型与业务约束纯函数。
 *
 * 本文件不依赖 SDK / Vue，全部可单测：
 * - 事务容器骨架的视图类型（详情/规则/操作/决议/阶梯）——结构语义属于内核，
 *   此处是客户端消费视图（从 sdk.affairs 的 readLog/readResolution/ladderStatus
 *   返回形状映射而来）；
 * - 业务约束纯函数（发起表单校验、时间线排序、能力判定、摘要、视图映射）。
 */

import { validateAffairRefs, type AffairRef } from '../../packages/plugin-sdk/src/affair-wire';

/** 参与阶梯级别（community-model.md §六「参与阶梯详设」默认值；与内核 tier 取值一致） */
export type LadderLevel = 'observer' | 'contributor' | 'voter';

/** 参与门槛声明：阶梯（你做过什么）或凭证（你是谁），由事务规则选择或组合 */
export type EntryRequirement =
  | { kind: 'none' }
  | { kind: 'ladder'; minLevel: LadderLevel }
  | { kind: 'credential'; credentialType: string };

/**
 * 创世规则（客户端草稿形态；落协议线形的映射见 wire.buildRulesDoc——
 * 发起人写死、之后只能走集体决策修改，发起人单方不可改）。
 */
export type AffairRules = {
  /** 贡献采纳/决议的公示期（小时）：吸收时钟偏差与副本滞后（§3.4）；协议下限 24h（§5.1） */
  reviewPeriodHours: number;
  /** 决议通过阈值：赞成占有效票（去弃权）比例 0-1（插件语义参数，内核不解释） */
  passThreshold: number;
  /** 法定人数：决议开始前最小投票者数（插件语义参数，内核不解释） */
  minQuorum: number;
  /** 冷启动：创世规则声明的初始投票者集合（须为 64 hex 身份 id；可空 = 仅发起人） */
  initialVoters: string[];
  /** 参与门槛（贡献/表决层；观察/评论层永远零门槛） */
  entryRequirement: EntryRequirement;
};

export type AffairCreateInput = {
  title: string;
  summary: string;
  tags: string[];
  /**
   * 事务间引用（affair.md §10 类型化暴露）：继承/申诉/父子/关联。
   * append-only 不可撤销；自指禁令由内核 enforced（affairId 复算后）。
   */
  refs: AffairRef[];
  /** 行政区域代码（GB/T 2260），仅检索聚合用，不代表机构认可（§六） */
  regionCode?: string;
  rules: AffairRules;
};

/** 事务墙列表项（落地 SDK 无 indexer 目录面，列表 = 本机关注的议题 + readLog 元数据） */
export type AffairListItem = {
  affairId: string;
  title: string;
  summary: string;
  tags: string[];
  /** 事务间引用（创世 refs 原文，§10） */
  refs: AffairRef[];
  /** 发起人身份 id（创世 initiator.identity；本参考实现为发起方插件域身份，见 wire.ts 头注） */
  originator: string;
  /** 发起人本地毫秒（创世声明值，仅展示用；权威时间见规格 §7） */
  createdAt: number;
  following: boolean;
  operationCount: number;
};

/** 议题详情（容器骨架：发起人 + 规则 + 元数据；内容由操作日志承载） */
export type AffairDetail = AffairListItem & {
  regionCode?: string;
  rules: AffairRules;
  /** 是否已关闭（存在生效/被否决的决议即视为关闭；以 readResolution 为准） */
  closed: boolean;
};

/** 操作类别：插件语义（wire 层 opType 恒为 content，类别在 payload.kind） */
export type AffairOperationKind = 'contribution' | 'vote' | 'comment';

export type AffairOperationInput = {
  kind: AffairOperationKind;
  payload: ContributionPayload | VotePayload | CommentPayload;
};

/**
 * 日志中的操作（从 sdk.affairs.readLog 的条目映射）。
 * 注意诚实口径：readLog 不暴露每条操作的存证锚定时刻，故展示时间为
 * declaredAt（签名者声明时刻，自报值，仅展示），不得当作链上时间呈现。
 */
export type AffairOperation = {
  opHash: string;
  prevOpHash: string;
  /** 操作者身份 id（协议 actor.identity；本参考实现为插件域身份，见 wire.ts 头注） */
  author: string;
  /** 签名者声明时刻（自报，仅展示） */
  declaredAt: number;
  kind: AffairOperationKind;
  payload: ContributionPayload | VotePayload | CommentPayload;
};

export type ContributionPayload = {
  /** 正式贡献：议案文本 / PR 链接 / 数据等（采纳标准由事务类型插件定义） */
  text: string;
};

export type VotePayload = {
  /** 投票对象：目标贡献或决议提议的操作哈希 */
  targetOpHash: string;
  choice: 'for' | 'against' | 'abstain';
  /** 票权身份：上下文身份（默认，跨事务不可关联）或公共身份（opt-in 公开履历） */
  identityMode: 'contextual' | 'public';
};

export type CommentPayload = {
  text: string;
};

/** 决议公示期状态（与 sdk.affairs readResolution 的 state 取值一致） */
export type AffairResolutionState = 'pending' | 'effective' | 'vetoed' | 'unanchored';

/**
 * 决议视图（从 sdk.affairs.readResolution 的条目映射）。
 * 决议是内核按公示期规则从操作集合 + 本副本存证链锚定时刻确定性推导的
 * 产物（§6.2）；result/condition/countedOps 为决议操作载荷原文（JSON），
 * 客户端只做展示。
 */
export type AffairResolutionView = {
  opHash: string;
  state: AffairResolutionState;
  /** 决议结果原文的展示形态（JSON 摘要） */
  resultText: string;
  objections: number;
  /** 本副本存证链锚定时刻；未锚定为 null（不得以声明时间冒充） */
  anchoredMs: number | null;
  pubPeriodMs: number;
};

/** 阶梯/账龄视图（单个参与者；从 sdk.affairs.ladderStatus 的条目映射） */
export type LadderState = {
  level: LadderLevel;
  accountAgeDays: number;
  adoptedContributions: number;
  daysAtCurrentLevel: number;
  lastActiveDaysAgo: number;
  /** 在级衰减：投票者 90 天无活动降回贡献者（默认值） */
  decayWarning: boolean;
};

/** 阶梯名册条目视图（ladderStatus.entries 的展示形态；一人一票不加权） */
export type LadderEntryView = {
  identity: string;
  level: LadderLevel;
  adoptedContributions: number;
  accountAgeDays: number | null;
  lastActiveDaysAgo: number | null;
};

// ------------------------------------------------------------------
// 规则版本链 / 执行状态 / 组织效力（sdk.affairs readRules / readExec /
// orgEffects 的客户端消费视图；结构语义属于内核，此处是展示映射）
// ------------------------------------------------------------------

/** 规则文档单个版本（§5.4「每一版本确定性可溯」） */
export type RulesVersionView = {
  seq: number;
  /** 版本依据（创世 = affairId；rule-change = 依据操作 opHash） */
  basisOpHash: string;
  rulesHash: string;
  /** 生效时刻（链上锚定 ms）；未生效为 null */
  effectiveMs: number | null;
};

/** 未生效 rule-change 条目的归宿（pending / rejected + 稳定 reason） */
export type RuleChangeView = {
  opHash: string;
  fate: 'pending' | 'rejected';
  reason: string;
};

/** 规则版本链视图（readRules 的展示形态） */
export type RulesChainView = {
  currentSeq: number;
  currentRulesHash: string;
  versions: RulesVersionView[];
  changes: RuleChangeView[];
};

/** 执行型事务状态机状态（§6.2-3 八态，与内核 ExecState 线形逐字对齐） */
export type ExecStateName =
  | 'unanchored'
  | 'resolution-pending'
  | 'resolution-vetoed'
  | 'awaiting-execution'
  | 'in-progress'
  | 'verifying'
  | 'returned'
  | 'closed';

/** 单决议的执行状态视图（readExec 的展示形态） */
export type ExecStateView = {
  resolutionOpHash: string;
  state: ExecStateName;
  reportOpHash: string | null;
  anchoredMs: number | null;
  effectiveMs: number | null;
};

/** 组织效力判定结果（org-genesis §6 三线判定） */
export type OrgEffectOutcome =
  | 'apply'
  | 'notDeclared'
  | 'revoked'
  | 'resolutionNotEffective'
  | 'grantNotAnchored'
  | 'resolutionNotAnchored'
  | 'notPrior';

/** 单条效力判定行视图（orgEffects 的展示形态） */
export type OrgEffectView = {
  scope: string;
  resolutionOpHash: string;
  outcome: OrgEffectOutcome;
  /** 回执状态（仅 outcome=apply 携带）：recorded = 已生效留痕 / unrecorded = 待应用 */
  receiptState: 'recorded' | 'unrecorded' | null;
};

/** 组织效力读出口视图（含复算无效被剔除的决议列表） */
export type OrgEffectsView = {
  effects: OrgEffectView[];
  invalidResolutions: string[];
};

/**
 * 效力应用编排报告（service.applyOrgEffects 的返回）：
 * - applied：本插件已完成内容应用的 scope（写回执的前提）；
 * - unapplied：未能应用的 scope + 原因（无写面/权限被拒/校验失败），
 *   存在 unapplied 时不写任何回执（fail-closed：回执 =「已生效」的机器
 *   可读凭据，未应用不得出具）；
 * - receiptActions：内核 applyOrgEffects 逐条动作（全部应用成功才存在）。
 */
export type OrgEffectApplyReport = {
  applied: string[];
  unapplied: Array<{ scope: string; resolutionOpHash: string; reason: string }>;
  receiptActions: Array<{ scope: string; resolutionOpHash: string; action: string }> | null;
};

// 协议上限（affair.md §2.1）：title 120 / summary 1024 / tags 16（单标签 32）。
// 此处客户端收紧为 80 / 500 / 8——客户端更严本身合法，但不要把这些值当协议线。
export const AFFAIR_TITLE_MAX_LENGTH = 80;
export const AFFAIR_SUMMARY_MAX_LENGTH = 500;
export const AFFAIR_TAG_MAX_COUNT = 8;
export const CONTRIBUTION_MAX_LENGTH = 2000;
export const COMMENT_MAX_LENGTH = 500;

/** 投票者衰减窗口（天），产品默认值（§六） */
export const VOTER_DECAY_DAYS = 90;

const MS_PER_DAY = 24 * 3600 * 1000;

export function validateAffairDraft(input: AffairCreateInput): { ok: boolean; reason?: string } {
  const title = input.title.trim();
  if (!title) {
    return { ok: false, reason: '标题不能为空' };
  }
  if (title.length > AFFAIR_TITLE_MAX_LENGTH) {
    return { ok: false, reason: `标题长度不能超过${AFFAIR_TITLE_MAX_LENGTH}字` };
  }
  if (!input.summary.trim()) {
    return { ok: false, reason: '简介不能为空' };
  }
  if (input.summary.trim().length > AFFAIR_SUMMARY_MAX_LENGTH) {
    return { ok: false, reason: `简介长度不能超过${AFFAIR_SUMMARY_MAX_LENGTH}字` };
  }
  if (input.tags.length > AFFAIR_TAG_MAX_COUNT) {
    return { ok: false, reason: `标签最多${AFFAIR_TAG_MAX_COUNT}个` };
  }
  if (input.tags.some((tag) => !tag.trim())) {
    return { ok: false, reason: '标签不能为空串' };
  }
  if (!(input.rules.passThreshold > 0 && input.rules.passThreshold <= 1)) {
    return { ok: false, reason: '通过阈值必须在 (0, 1] 区间' };
  }
  if (input.rules.minQuorum < 1) {
    return { ok: false, reason: '法定人数至少为 1' };
  }
  // 协议下限 24h（§5.1）：低于下限的创世记录会被内核静态检查拒绝，提前在表单拦截
  if (input.rules.reviewPeriodHours < 24) {
    return { ok: false, reason: '公示期不能低于 24 小时（协议下限，吸收时钟偏差与副本滞后）' };
  }
  if (input.rules.initialVoters.some((id) => !/^[0-9a-f]{64}$/.test(id))) {
    return { ok: false, reason: '初始投票者必须是 64 位小写 hex 身份 id' };
  }
  // 事务间引用（§10）：形状校验（rel 枚举 + target 64 hex）；自指禁令归内核
  const refsVerdict = validateAffairRefs(input.refs ?? []);
  if (!refsVerdict.ok) {
    return { ok: false, reason: refsVerdict.reason };
  }
  return { ok: true };
}

export function validateContributionText(text: string): { ok: boolean; reason?: string } {
  const normalized = text.trim();
  if (!normalized) {
    return { ok: false, reason: '贡献内容不能为空' };
  }
  if (normalized.length > CONTRIBUTION_MAX_LENGTH) {
    return { ok: false, reason: `贡献内容不能超过${CONTRIBUTION_MAX_LENGTH}字` };
  }
  return { ok: true };
}

export function validateCommentText(text: string): { ok: boolean; reason?: string } {
  const normalized = text.trim();
  if (!normalized) {
    return { ok: false, reason: '评论不能为空' };
  }
  if (normalized.length > COMMENT_MAX_LENGTH) {
    return { ok: false, reason: `评论不能超过${COMMENT_MAX_LENGTH}字` };
  }
  return { ok: true };
}

/**
 * 时间线排序纪律（§3.3 诚实边界）：readLog 不暴露逐条操作的存证锚定时刻，
 * 且锚定时刻是本机锚定的到达函数、跨副本不可复现——故本函数只服务展示层：
 * 按签名者声明时刻（自报值）升序、同刻按 opHash 字典序tie-break，不声称
 * 「任何副本排出同一顺序」。凡计数/判定场景一律以 opHash 字典序为确定性键
 * （affair.md §8；sdk.affairs.readLog 返回的 ops 即按此序）。
 */
export function sortOperations(operations: AffairOperation[]): AffairOperation[] {
  return [...operations].sort((a, b) => {
    if (a.declaredAt !== b.declaredAt) {
      return a.declaredAt - b.declaredAt;
    }
    return a.opHash < b.opHash ? -1 : a.opHash > b.opHash ? 1 : 0;
  });
}

/**
 * 阶梯能力判定（§六详设默认值）：观察/评论层永远零门槛；贡献需贡献者级；
 * 表决需投票者级。门槛按 entryRequirement 收紧（凭证类门槛客户端只提示，
 * 资格判定以内核/凭证验证为准）。
 */
export function canComment(): boolean {
  return true;
}

export function canSubmitContribution(level: LadderLevel): boolean {
  return level === 'contributor' || level === 'voter';
}

export function canVote(level: LadderLevel): boolean {
  return level === 'voter';
}

/** 决议计票（纯函数，插件语义演示）：赞成占有效票（去弃权）比例 >= 阈值 且 票数达法定人数 */
export function evaluateTally(
  tally: { for: number; against: number; abstain: number },
  rules: Pick<AffairRules, 'passThreshold' | 'minQuorum'>
): 'passed' | 'rejected' {
  const ballots = tally.for + tally.against;
  if (ballots < rules.minQuorum) {
    return 'rejected';
  }
  return tally.for / ballots >= rules.passThreshold ? 'passed' : 'rejected';
}

/** 应用消息摘要（p2p-messages §20 强制 summary；未装插件设备原生渲染此文本） */
export function buildAffairSummary(title: string): string {
  const preview = title.trim().slice(0, 40);
  return `【新议题】${preview}`;
}

// ------------------------------------------------------------------
// SDK 返回形状 → 视图模型的映射（纯函数；原始记录为宽松 JSON）
// ------------------------------------------------------------------

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/** 创世记录 → 列表/详情元数据；结构不符（未通过内核校验的形状）返回 null */
export function readGenesisMeta(
  genesis: unknown
): { title: string; summary: string; tags: string[]; refs: AffairRef[]; originator: string; createdAt: number; regionCode?: string } | null {
  const record = asRecord(genesis);
  const title = asString(record?.title);
  const initiator = asRecord(record?.initiator);
  const originator = asString(initiator?.identity);
  const createdAt = asNumber(record?.createdAt);
  if (!record || title === null || originator === null || createdAt === null) {
    return null;
  }
  const tags = Array.isArray(record.tags) ? record.tags.filter((tag): tag is string => typeof tag === 'string') : [];
  // 事务间引用（§10）：宽松读取（内核已做线形校验；形状不全的条目跳过不展示）
  const refs: AffairRef[] = Array.isArray(record.refs)
    ? record.refs.flatMap((item) => {
        const entry = asRecord(item);
        const target = asString(entry?.target);
        const rel = asString(entry?.rel);
        return target && (rel === 'inherit' || rel === 'appeal' || rel === 'parent' || rel === 'related')
          ? [{ target, rel }]
          : [];
      })
    : [];
  const meta = {
    title,
    summary: asString(record.summary) ?? '',
    tags,
    refs,
    originator,
    createdAt
  };
  const regionCode = asString(record.regionCode);
  return regionCode ? { ...meta, regionCode } : meta;
}

/** 创世规则文档 → 客户端规则视图（插件语义参数缺席时回退产品默认值） */
export function rulesFromGenesis(genesis: unknown): AffairRules {
  const record = asRecord(genesis);
  const rules = asRecord(record?.rules);
  const pubPeriod = asRecord(rules?.pubPeriod);
  const delayMs = asNumber(pubPeriod?.delayMs);
  const pluginParams = asRecord(rules?.sparkAffairs);
  const participation = asRecord(rules?.participation);
  const contributeLadder = asString(asRecord(participation?.contribute)?.ladder);
  const credentials = Array.isArray(participation?.credentials) ? participation.credentials : [];
  const firstCredential = asRecord(credentials[0]);
  const initialVoters = Array.isArray(record?.initialVoters)
    ? record.initialVoters.filter((id): id is string => typeof id === 'string')
    : [];
  const entryRequirement: EntryRequirement =
    contributeLadder === 'observer' || contributeLadder === 'contributor' || contributeLadder === 'voter'
      ? { kind: 'ladder', minLevel: contributeLadder }
      : asString(firstCredential?.credType)
        ? { kind: 'credential', credentialType: asString(firstCredential?.credType) as string }
        : { kind: 'none' };
  return {
    reviewPeriodHours: delayMs !== null ? delayMs / MS_PER_DAY * 24 : 24,
    passThreshold: asNumber(pluginParams?.passThreshold) ?? 0.67,
    minQuorum: asNumber(pluginParams?.minQuorum) ?? 1,
    initialVoters,
    entryRequirement
  };
}

/** readLog 操作条目 → 时间线视图；非 content 类别或结构不符返回 null（展示层跳过） */
export function toAffairOperation(opHash: string, op: unknown): AffairOperation | null {
  const record = asRecord(op);
  const payload = asRecord(record?.payload);
  const kind = asString(payload?.kind);
  const actor = asRecord(record?.actor);
  const author = asString(actor?.identity);
  const declaredAt = asNumber(record?.declaredAt);
  const prevOpHash = asString(record?.prevOpHash);
  if (!record || !payload || author === null || declaredAt === null || prevOpHash === null) {
    return null;
  }
  if (kind !== 'contribution' && kind !== 'vote' && kind !== 'comment') {
    return null;
  }
  const { kind: _dropped, ...rest } = payload;
  return { opHash, prevOpHash, author, declaredAt, kind, payload: rest as AffairOperation['payload'] };
}

/** readResolution 条目 → 决议视图；结构不符返回 null */
export function toResolutionView(raw: unknown): AffairResolutionView | null {
  const record = asRecord(raw);
  const opHash = asString(record?.opHash);
  const state = asString(record?.state);
  const objections = asNumber(record?.objections);
  const pubPeriodMs = asNumber(record?.pubPeriodMs);
  if (
    !record ||
    opHash === null ||
    (state !== 'pending' && state !== 'effective' && state !== 'vetoed' && state !== 'unanchored') ||
    objections === null ||
    pubPeriodMs === null
  ) {
    return null;
  }
  return {
    opHash,
    state,
    resultText: JSON.stringify(record.result ?? null),
    objections,
    anchoredMs: asNumber(record.anchoredMs),
    pubPeriodMs
  };
}

/** ladderStatus 名册条目 → 展示视图；结构不符返回 null */
export function toLadderEntryView(raw: unknown, nowMs: number): LadderEntryView | null {
  const record = asRecord(raw);
  const identity = asString(record?.identity);
  const tier = asString(record?.tier);
  if (!record || identity === null || (tier !== 'observer' && tier !== 'contributor' && tier !== 'voter')) {
    return null;
  }
  const accountAgeMs = asNumber(record.accountAgeMs);
  const lastActivityMs = asNumber(record.lastActivityMs);
  return {
    identity,
    level: tier,
    adoptedContributions: asNumber(record.accepts) ?? 0,
    accountAgeDays: accountAgeMs !== null ? Math.floor(accountAgeMs / MS_PER_DAY) : null,
    lastActiveDaysAgo: lastActivityMs !== null ? Math.floor((nowMs - lastActivityMs) / MS_PER_DAY) : null
  };
}

/** ladderStatus 名册条目 → 单参与者阶梯视图（「我的阶梯状态」卡片） */
export function toLadderState(raw: unknown, nowMs: number): LadderState | null {
  const entry = toLadderEntryView(raw, nowMs);
  if (!entry) {
    return null;
  }
  const record = asRecord(raw);
  const tierSinceMs = asNumber(record?.tierSinceMs);
  const lastActiveDaysAgo = entry.lastActiveDaysAgo ?? Number.MAX_SAFE_INTEGER;
  return {
    level: entry.level,
    accountAgeDays: entry.accountAgeDays ?? 0,
    adoptedContributions: entry.adoptedContributions,
    daysAtCurrentLevel: tierSinceMs !== null ? Math.max(0, Math.floor((nowMs - tierSinceMs) / MS_PER_DAY)) : 0,
    lastActiveDaysAgo,
    decayWarning: entry.level === 'voter' && lastActiveDaysAgo >= VOTER_DECAY_DAYS
  };
}

/** readRules 返回 → 规则版本链视图；结构不符返回 null（不编造版本链） */
export function toRulesChainView(raw: unknown): RulesChainView | null {
  const record = asRecord(raw);
  const current = asRecord(record?.current);
  const currentSeq = asNumber(current?.seq);
  const currentRulesHash = asString(current?.rulesHash);
  if (!record || currentSeq === null || currentRulesHash === null) {
    return null;
  }
  const versions = (Array.isArray(record.versions) ? record.versions : []).flatMap((item) => {
    const entry = asRecord(item);
    const seq = asNumber(entry?.seq);
    const basisOpHash = asString(entry?.basisOpHash);
    const rulesHash = asString(entry?.rulesHash);
    return entry && seq !== null && basisOpHash !== null && rulesHash !== null
      ? [{ seq, basisOpHash, rulesHash, effectiveMs: asNumber(entry.effectiveMs) }]
      : [];
  });
  const changes = (Array.isArray(record.changes) ? record.changes : []).flatMap((item) => {
    const entry = asRecord(item);
    const opHash = asString(entry?.opHash);
    const fate = asString(entry?.fate);
    const reason = asString(entry?.reason);
    return entry && opHash !== null && (fate === 'pending' || fate === 'rejected') && reason !== null
      ? [{ opHash, fate, reason }]
      : [];
  });
  return { currentSeq, currentRulesHash, versions, changes };
}

const EXEC_STATE_NAMES: readonly ExecStateName[] = [
  'unanchored',
  'resolution-pending',
  'resolution-vetoed',
  'awaiting-execution',
  'in-progress',
  'verifying',
  'returned',
  'closed'
];

/** readExec 返回 → 执行状态视图列表；非执行型事务（exec == null）返回空列表 */
export function toExecStateViews(raw: unknown): ExecStateView[] {
  const record = asRecord(raw);
  if (!record || record.exec === null || record.exec === undefined || !Array.isArray(record.states)) {
    return [];
  }
  return record.states.flatMap((item) => {
    const entry = asRecord(item);
    const resolutionOpHash = asString(entry?.resolutionOpHash);
    const state = asString(entry?.state);
    return entry &&
      resolutionOpHash !== null &&
      state !== null &&
      (EXEC_STATE_NAMES as readonly string[]).includes(state)
      ? [
          {
            resolutionOpHash,
            state: state as ExecStateName,
            reportOpHash: asString(entry.reportOpHash),
            anchoredMs: asNumber(entry.anchoredMs),
            effectiveMs: asNumber(entry.effectiveMs)
          }
        ]
      : [];
  });
}

const ORG_EFFECT_OUTCOMES: readonly OrgEffectOutcome[] = [
  'apply',
  'notDeclared',
  'revoked',
  'resolutionNotEffective',
  'grantNotAnchored',
  'resolutionNotAnchored',
  'notPrior'
];

/** orgEffects 返回 → 组织效力视图；结构不符返回 null */
export function toOrgEffectsView(raw: unknown): OrgEffectsView | null {
  const record = asRecord(raw);
  if (!record || !Array.isArray(record.effects)) {
    return null;
  }
  const effects = record.effects.flatMap((item) => {
    const entry = asRecord(item);
    const scope = asString(entry?.scope);
    const resolutionOpHash = asString(entry?.resolutionOpHash);
    const outcome = asString(entry?.outcome);
    if (
      !entry ||
      scope === null ||
      resolutionOpHash === null ||
      outcome === null ||
      !(ORG_EFFECT_OUTCOMES as readonly string[]).includes(outcome)
    ) {
      return [];
    }
    const receiptState = asString(asRecord(entry.receipt)?.state);
    return [
      {
        scope,
        resolutionOpHash,
        outcome: outcome as OrgEffectOutcome,
        receiptState:
          receiptState === 'recorded' || receiptState === 'unrecorded'
            ? (receiptState as 'recorded' | 'unrecorded')
            : null
      }
    ];
  });
  const invalidResolutions = Array.isArray(record.invalidResolutions)
    ? record.invalidResolutions.filter((id): id is string => typeof id === 'string')
    : [];
  return { effects, invalidResolutions };
}
