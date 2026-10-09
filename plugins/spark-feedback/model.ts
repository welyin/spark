/**
 * 问题反馈应用（spark-feedback）· 数据模型与纯函数层（feedback.md v0.2 §3）。
 *
 * 分层口径（同 spark-git-repo/model.ts）：本文件不依赖 SDK 运行时/Vue，全部可单测。
 * - 回流产物（事务面）：子事务创世载荷 = { title, body, environment?, attachments?,
 *   feedbackChannel }，type 取 'bug' | 'proposal'（对齐「项目」插件 affairTypes，
 *   本插件只生产不注册，X8 口径）；
 * - 个人侧数据（§3.2）：草稿 local / 台账 append-only sync / 偏好 lww sync——
 *   集合声明在 service.ts，本文件只定义记录形状与校验；
 * - 环境信息（档二-9）：桥握手 ctx 注入 appVersion/platform/shellVersion，插件只读；
 *   旧壳层字段缺省时按 undefined 兼容降级（可退回用户手填版本号）；
 * - 每日提交数温馨提示（档三-18）：纯客户端自律提示，不设硬门槛。
 */

import type { PluginContext, PluginPlatform } from '../../packages/plugin-sdk/src';

// ------------------------------------------------------------------
// 常量
// ------------------------------------------------------------------

/** 反馈类型（对齐「项目」插件子事务类型清单；本插件只生产不注册） */
export const FEEDBACK_TYPES = ['bug', 'proposal'] as const;
export type FeedbackType = (typeof FEEDBACK_TYPES)[number];

/** 自述性元数据（§5.2 反向不可见：项目侧不依赖它做任何判定） */
export const FEEDBACK_CHANNEL = 'spark-feedback/0.1.0';

/**
 * 内置 Spark 项目议题 affairId（档三-15 三路兜底之一）。
 * Spark 自身项目议题尚未创世发布——占位空串 = 未签发，目标解析时跳过该路；
 * 议题创世后将真实 affairId（64 位小写 hex）回填本常量即生效。
 */
export const BUILTIN_SPARK_AFFAIR_ID = '';

/** 每日提交数温馨提示阈值（档三-18：温馨提示，不阻断；硬门槛归目标议题规则文档） */
export const DAILY_GENTLE_LIMIT = 10;

/** 输入校验边界（客户端自律；权威校验在目标议题规则侧） */
export const TITLE_MIN = 2;
export const TITLE_MAX = 80;
export const BODY_MIN = 10;
export const BODY_MAX = 4000;
export const REPRO_MAX = 4000;
export const ATTACHMENT_MAX = 8;
/** 附件体积提示阈值（档三-7 同口径：提示阈值，硬上限复用内核体积卫生机制） */
export const ATTACHMENT_SIZE_HINT = 10 * 1024 * 1024;

/** 子事务类型标识（与「项目」插件文档约定；契约落地后以提供方 schema 为准） */
export function affairTypeOf(type: FeedbackType): string {
  return type;
}

// ------------------------------------------------------------------
// 记录形状
// ------------------------------------------------------------------

/** 环境信息（创世载荷 environment 段；全部字段低敏，ctx 缺什么省什么） */
export type EnvironmentInfo = {
  appVersion?: string;
  platform?: PluginPlatform;
  shellVersion?: string;
  /** 提交时的空间上下文（档三-17：个人/组织） */
  spaceKind: 'personal' | 'org';
  /** 用户手填版本号（ctx 注入缺省时的降级通道，档二-9 注入前口径） */
  reportedVersion?: string;
};

/** 附件描述（cid = SHA-256 内容寻址，sdk.content.saveBlob 保存即声明 provider） */
export type FeedbackAttachment = {
  cid: string;
  name: string;
  size: number;
  mime?: string;
};

/** 回流子事务创世载荷（§3.1；置于创世记录 extra.feedback 下） */
export type FeedbackPayload = {
  body: string;
  /** 复现步骤（缺陷类型建议填写） */
  reproduction?: string;
  environment?: EnvironmentInfo;
  attachments?: FeedbackAttachment[];
  feedbackChannel: string;
};

/** 反馈草稿（feedback_drafts，lww-record，scope local——草稿是设备现场，多设备不同步） */
export type FeedbackDraft = {
  id: string;
  type: FeedbackType;
  title: string;
  body: string;
  reproduction: string;
  /** 已上传附件（cid 级引用；本地暂存未上传的候选件不入草稿） */
  attachments: FeedbackAttachment[];
  /** 目标议题（用户已选；空 = 未选） */
  targetAffairId: string;
  savedAt: number;
};

/** 反馈台账条目（feedback_ledger，append-only，个人空间自设备间同步） */
export type LedgerEntry = {
  id: string;
  targetAffairId: string;
  childAffairId: string;
  type: FeedbackType;
  title: string;
  submittedAt: number;
};

/** 偏好配置（lww-record，sync 个人空间） */
export type FeedbackPrefs = {
  /** 默认目标议题 affairId（空/缺省 = 未设置） */
  defaultTargetAffairId?: string;
  /** 环境信息附带开关（缺省 true：表单默认勾选但逐次可见可关） */
  includeEnvironment?: boolean;
};

/** 表单输入（提交/导出共用） */
export type FeedbackInput = {
  type: FeedbackType;
  title: string;
  body: string;
  reproduction?: string;
  includeEnvironment: boolean;
  attachments: FeedbackAttachment[];
  /** 用户手填版本号（仅 ctx 环境信息缺省时生效） */
  reportedVersion?: string;
};

/** 「我的反馈」列表条目：台账 + 本地副本所见的子事务决议状态（§4 如实呈现） */
export type LedgerView = LedgerEntry & {
  /**
   * 本地副本所见状态：
   * - 'effective'：已有决议生效（采纳/关闭结论以「项目」插件呈现为准）；
   * - 'pending'：有决议在公示期（含未锚定）；
   * - 'vetoed'：所见决议均被否决（决议流程已有结论）；
   * - 'none'：本地副本未见后续决议（如实标注「暂无后续」，不编造状态）；
   * - 'unavailable'：子事务本地副本不可读（未同步到）。
   */
  status: 'effective' | 'pending' | 'vetoed' | 'none' | 'unavailable';
};

// ------------------------------------------------------------------
// 校验与构造（纯函数）
// ------------------------------------------------------------------

export function isValidAffairId(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
}

/** 表单输入校验（提交与导出同一道关；返回首个失败原因） */
export function validateFeedbackInput(input: FeedbackInput): { ok: boolean; reason?: string } {
  if (!FEEDBACK_TYPES.includes(input.type)) {
    return { ok: false, reason: '反馈类型必须是 bug（缺陷）或 proposal（建议）' };
  }
  const title = input.title.trim();
  if (title.length < TITLE_MIN || title.length > TITLE_MAX) {
    return { ok: false, reason: `标题长度须在 ${TITLE_MIN}–${TITLE_MAX} 字符之间` };
  }
  const body = input.body.trim();
  if (body.length < BODY_MIN || body.length > BODY_MAX) {
    return { ok: false, reason: `描述长度须在 ${BODY_MIN}–${BODY_MAX} 字符之间（当前 ${body.length}）` };
  }
  const repro = input.reproduction?.trim() ?? '';
  if (repro.length > REPRO_MAX) {
    return { ok: false, reason: `复现步骤长度不能超过 ${REPRO_MAX} 字符` };
  }
  if (input.attachments.length > ATTACHMENT_MAX) {
    return { ok: false, reason: `附件最多 ${ATTACHMENT_MAX} 件` };
  }
  for (const attachment of input.attachments) {
    if (!isValidAffairId(attachment.cid)) {
      return { ok: false, reason: `附件 ${attachment.name} 的 cid 形状非法（须为 64 位小写 hex）` };
    }
  }
  return { ok: true };
}

/**
 * 组装环境信息（档二-9）：ctx 注入字段缺什么省什么；spaceKind 恒有值。
 * includeEnvironment=false → null（不附带）；全部字段缺省且无手填 → 仅 spaceKind。
 */
export function buildEnvironment(
  ctx: Pick<PluginContext, 'space'> & Partial<Pick<PluginContext, 'appVersion' | 'platform' | 'shellVersion'>>,
  reportedVersion?: string
): EnvironmentInfo {
  const env: EnvironmentInfo = { spaceKind: ctx.space.type };
  if (typeof ctx.appVersion === 'string' && ctx.appVersion) {
    env.appVersion = ctx.appVersion;
  }
  if (typeof ctx.platform === 'string' && ctx.platform) {
    env.platform = ctx.platform;
  }
  if (typeof ctx.shellVersion === 'string' && ctx.shellVersion) {
    env.shellVersion = ctx.shellVersion;
  }
  const reported = reportedVersion?.trim();
  if (reported) {
    env.reportedVersion = reported;
  }
  return env;
}

/** ctx 环境信息是否可用（不可用 → 视图层给手填版本号降级入口） */
export function hasBridgeEnvironment(ctx: Partial<Pick<PluginContext, 'appVersion' | 'platform' | 'shellVersion'>>): boolean {
  return Boolean(ctx.appVersion || ctx.platform || ctx.shellVersion);
}

/** 组装回流载荷（§3.1；空字段不携带，canonical 保持最小） */
export function buildFeedbackPayload(
  input: FeedbackInput,
  environment: EnvironmentInfo | null
): FeedbackPayload {
  const payload: FeedbackPayload = {
    body: input.body.trim(),
    feedbackChannel: FEEDBACK_CHANNEL
  };
  const repro = input.reproduction?.trim();
  if (repro) {
    payload.reproduction = repro;
  }
  if (input.includeEnvironment && environment) {
    payload.environment = environment;
  }
  if (input.attachments.length > 0) {
    payload.attachments = input.attachments.map((a) => ({
      cid: a.cid,
      name: a.name,
      size: a.size,
      ...(a.mime ? { mime: a.mime } : {})
    }));
  }
  return payload;
}

// ------------------------------------------------------------------
// 每日提交数温馨提示（档三-18）
// ------------------------------------------------------------------

/** 本地日历日边界（ms）：[start, end) */
export function localDayRange(nowMs: number): { start: number; end: number } {
  const day = new Date(nowMs);
  day.setHours(0, 0, 0, 0);
  const start = day.getTime();
  return { start, end: start + 86_400_000 };
}

export function countTodaySubmissions(entries: Array<Pick<LedgerEntry, 'submittedAt'>>, nowMs: number): number {
  const { start, end } = localDayRange(nowMs);
  return entries.filter((entry) => entry.submittedAt >= start && entry.submittedAt < end).length;
}

/** 温馨提示文案（未达阈值 → null；提示不阻断，硬门槛归目标议题规则文档） */
export function dailyLimitNotice(todayCount: number): string | null {
  if (todayCount < DAILY_GENTLE_LIMIT) {
    return null;
  }
  return `你今天已提交 ${todayCount} 条反馈。为维护议题讨论质量，建议先检索是否已有同类反馈再提交；是否继续由你决定（频次硬门槛归目标议题规则文档）。`;
}

// ------------------------------------------------------------------
// 台账 / 草稿记录的读侧解析（防御性：同步面数据不假设完整）
// ------------------------------------------------------------------

export function parseLedgerEntry(value: unknown): LedgerEntry | null {
  const record = value as Record<string, unknown> | null;
  if (!record || typeof record !== 'object') {
    return null;
  }
  if (
    typeof record.id !== 'string' ||
    !isValidAffairId(record.targetAffairId) ||
    !isValidAffairId(record.childAffairId) ||
    typeof record.title !== 'string' ||
    typeof record.submittedAt !== 'number' ||
    !FEEDBACK_TYPES.includes(record.type as FeedbackType)
  ) {
    return null;
  }
  return {
    id: record.id,
    targetAffairId: record.targetAffairId,
    childAffairId: record.childAffairId,
    type: record.type as FeedbackType,
    title: record.title,
    submittedAt: record.submittedAt
  };
}

export function parseDraft(id: string, value: unknown): FeedbackDraft | null {
  const record = value as Record<string, unknown> | null;
  if (!record || typeof record !== 'object') {
    return null;
  }
  if (!FEEDBACK_TYPES.includes(record.type as FeedbackType)) {
    return null;
  }
  const attachments = Array.isArray(record.attachments)
    ? record.attachments.filter(
        (a): a is FeedbackAttachment =>
          typeof a === 'object' && a !== null && isValidAffairId((a as FeedbackAttachment).cid)
          && typeof (a as FeedbackAttachment).name === 'string' && typeof (a as FeedbackAttachment).size === 'number'
      )
    : [];
  return {
    id,
    type: record.type as FeedbackType,
    title: typeof record.title === 'string' ? record.title : '',
    body: typeof record.body === 'string' ? record.body : '',
    reproduction: typeof record.reproduction === 'string' ? record.reproduction : '',
    attachments,
    targetAffairId: isValidAffairId(record.targetAffairId) ? record.targetAffairId : '',
    savedAt: typeof record.savedAt === 'number' ? record.savedAt : 0
  };
}

export function parsePrefs(value: unknown): FeedbackPrefs {
  const record = value as Record<string, unknown> | null;
  if (!record || typeof record !== 'object') {
    return {};
  }
  return {
    ...(isValidAffairId(record.defaultTargetAffairId) ? { defaultTargetAffairId: record.defaultTargetAffairId } : {}),
    ...(typeof record.includeEnvironment === 'boolean' ? { includeEnvironment: record.includeEnvironment } : {})
  };
}

// ------------------------------------------------------------------
// 决议状态推导（本地副本所见，§4 如实呈现）
// ------------------------------------------------------------------

/** 从决议集合推导台账展示状态（任何节点对同一副本集合复算一致） */
export function statusFromResolutionStates(states: Array<'pending' | 'effective' | 'vetoed' | 'unanchored'>): LedgerView['status'] {
  if (states.some((s) => s === 'effective')) {
    return 'effective';
  }
  if (states.some((s) => s === 'pending' || s === 'unanchored')) {
    return 'pending';
  }
  if (states.length > 0) {
    return 'vetoed';
  }
  return 'none';
}

export const STATUS_LABELS: Record<LedgerView['status'], string> = {
  effective: '已有决议生效',
  pending: '决议公示中',
  vetoed: '决议被否决',
  none: '暂无后续',
  unavailable: '本地副本未同步'
};
