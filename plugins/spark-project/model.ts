/**
 * 项目（议题）插件（spark-project）· 数据模型与纯函数。
 *
 * 设计依据：wiki/product/bootstrap-plugins/project.md v0.2 + decisions.md 拍板口径。
 *
 * 分层总览（§3）：
 * - 项目议题本体 = 一条创世记录 + 操作日志（sdk.affairs 承载）；bug/建议/PR
 *   子事务走父子引用（refs rel=parent，§3.4）；
 * - 子事务处置（采纳/关闭）双形态（R1 拍板选 a）：
 *   bug/proposal/通用 = 维护者签名处置动议（content + project.disposition）
 *   + 决议操作（opType=resolution，condition 逐字回引规则 closeConditions，
 *   countedOps=[动议 opHash]）入内核公示期——公示期内决议徽标「待确认」，
 *   公示期满无阈值异议转「已生效」（affair.md §6.1/§6.2 线形，内核逐副本
 *   复算 countedOps/rulesHash/pubPeriod）；PR 维持档一-3：单维护者合并回执
 *   （content op）即生效，不走公示期；两形态读侧都按 parent 项目议题规则
 *   文档 rules.maintainers 写权过滤（与 spark-kanban 状态操作、
 *   spark-git-repo PR 终态同一约定）；
 * - 本插件独占注册 project/bug/proposal/pr 四类事务类型（X8；其余插件只生产
 *   不注册）。
 *
 * 拍板落点：
 * - maintainers 登记插件域身份 id（decisions 实施期补录口径）；
 * - 排序红线：declaredAt 永不参与判定排序（因果序 prevOpHash 祖先闭包 +
 *   opHash 字典序 tie-break，与内核 ancestor_op_hashes 同构；展示序另算）；
 * - 档三-4：未预设子事务类型显示为通用子事务；
 * - 档三-5：归档 = 停止公开索引 + 只读标注（MVP 仅此）。
 *
 * 本文件不依赖 SDK 运行时/Vue，全部可单测。
 */

// ------------------------------------------------------------------
// 常量与类型枚举
// ------------------------------------------------------------------

/** 本插件独占注册的事务类型（manifest affairTypes，X8） */
export const PROJECT_AFFAIR_TYPE = 'project';
export const CHILD_AFFAIR_TYPES = ['bug', 'proposal', 'pr'] as const;
export type ChildAffairType = (typeof CHILD_AFFAIR_TYPES)[number];

export const CHILD_TYPE_LABELS: Record<string, string> = {
  bug: '缺陷',
  proposal: '建议',
  pr: 'PR'
};

/** 操作载荷 kind（opType 恒为 content，插件语义在 payload.kind，内核不解释） */
export const PROJECT_COMMENT_KIND = 'project.comment';
export const CHILD_NOTICE_KIND = 'project.child-notice';
export const DISPOSITION_KIND = 'project.disposition';

/**
 * 处置关闭条件（R1：affair.md §5.2 op-count 形态，内核可判定）：
 * 一条匹配的处置动议（content + payload.kind=project.disposition）即满足
 * 关闭门槛——「单维护者处置动议 + 公示期」由内核逐副本确定性复算
 * （countedOps = 闭包内匹配操作按 §8 opHash 升序前 1 条）。
 */
export function buildDispositionCloseCondition(): Record<string, unknown> {
  return { type: 'op-count', opType: 'content', filter: DISPOSITION_KIND, count: 1 };
}

/** 子事务处置动作（档一-3：单维护者回执即生效；delayed-veto/m-n 排后续迭代） */
export const DISPOSITION_ACTIONS = ['adopted', 'closed'] as const;
export type DispositionAction = (typeof DISPOSITION_ACTIONS)[number];

export const DISPOSITION_LABELS: Record<DispositionAction, string> = {
  adopted: '已采纳',
  closed: '已关闭'
};

/** 校验上限 */
export const PROJECT_MAX_TITLE_LENGTH = 120;
export const PROJECT_MAX_SUMMARY_LENGTH = 2000;
export const PROJECT_MAX_TAGS = 8;
export const PROJECT_MAX_TAG_LENGTH = 24;
export const PROJECT_MAX_COMMENT_LENGTH = 5000;
export const PROJECT_MAX_DOC_TITLE_LENGTH = 120;
export const PROJECT_MAX_DOC_BODY_LENGTH = 50000;

/** 公示期协议下限 = 24h（affair.md §5.1；与 spark-affairs 同口径） */
export const PROJECT_PUB_PERIOD_MS = 24 * 3600 * 1000;

/** 规则模板标识（§7：MVP 只做「维护者制」一套，不做规则编辑器） */
export const PROJECT_RULES_TEMPLATE = 'maintainer-led';

// ------------------------------------------------------------------
// 视图类型
// ------------------------------------------------------------------

/** 项目议题元数据（创世记录读侧投影；结构不符 → null，不编造占位） */
export type ProjectMeta = {
  affairId: string;
  type: string;
  title: string;
  summary: string;
  tags: string[];
  /** 公开发布声明位（档二-2：创世顶层严格布尔 publish:true） */
  isPublic: boolean;
  createdAt: number;
  following: boolean;
  operationCount: number;
};

/** 子事务处置状态（读侧推导；open = 尚无有效处置操作） */
export type DispositionState = 'open' | DispositionAction;

export type DispositionView = {
  state: DispositionState;
  /** 最近一次有效处置操作 opHash（open 为 null） */
  opHash: string | null;
  note: string | null;
  actorIdentity: string | null;
};

/** 子事务列表项（决议徽标原样呈现公示期状态，不美化） */
export type ChildAffairView = {
  affairId: string;
  /** bug/proposal/pr 或通用（档三-4 未预设类型原样显示） */
  type: string;
  typeLabel: string;
  title: string;
  summary: string;
  createdAt: number;
  disposition: DispositionView;
  resolutionBadge: 'open' | 'pending' | 'effective' | 'vetoed';
};

/** 讨论流时间线条目（发言/子事务通告/处置/决议同流呈现，§4） */
export type TimelineEntry = {
  opHash: string;
  kind: 'comment' | 'child-notice' | 'disposition' | 'resolution' | 'other';
  text: string;
  actorIdentity: string | null;
  /** 签名者声明时刻（自报值，仅展示序使用，永不进判定） */
  declaredAt: number;
  /** disposition 条目携带动作 */
  action?: DispositionAction;
};

/** 项目文档版本（append-only 集合记录；版本链由插件语义管理，§3.2） */
export type ProjectDocVersion = {
  projectAffairId: string;
  docId: string;
  seq: number;
  title: string;
  body: string;
  authorIdentity: string;
  createdAt: number;
};

/** 文档摘要（版本链折叠后的读侧投影） */
export type ProjectDocSummary = {
  docId: string;
  title: string;
  latestSeq: number;
  updatedAt: number;
  versionCount: number;
};

/** 成员页名册条目（内核阶梯推导原样呈现 + 维护者标注） */
export type ProjectMemberView = {
  identity: string;
  tier: 'observer' | 'contributor' | 'voter';
  accepts: number;
  accountAgeMs: number | null;
  lastActivityMs: number | null;
  isMaintainer: boolean;
};

// ------------------------------------------------------------------
// 校验
// ------------------------------------------------------------------

export function normalizeProjectText(text: string): string {
  return text.trim();
}

export function isValidAffairId(value: string): boolean {
  return /^[0-9a-f]{64}$/.test(value);
}

export function validateProjectInput(input: {
  title: string;
  summary: string;
  tags: string[];
}): { ok: boolean; reason?: string } {
  const title = normalizeProjectText(input.title);
  if (!title) {
    return { ok: false, reason: '项目标题不能为空' };
  }
  if (title.length > PROJECT_MAX_TITLE_LENGTH) {
    return { ok: false, reason: `项目标题过长（上限 ${PROJECT_MAX_TITLE_LENGTH} 字符）` };
  }
  const summary = normalizeProjectText(input.summary);
  if (!summary) {
    return { ok: false, reason: '项目简介不能为空（公共目录索引三要素之一）' };
  }
  if (summary.length > PROJECT_MAX_SUMMARY_LENGTH) {
    return { ok: false, reason: `项目简介过长（上限 ${PROJECT_MAX_SUMMARY_LENGTH} 字符）` };
  }
  if (input.tags.length > PROJECT_MAX_TAGS) {
    return { ok: false, reason: `标签过多（上限 ${PROJECT_MAX_TAGS} 个）` };
  }
  for (const tag of input.tags) {
    const normalized = normalizeProjectText(tag);
    if (!normalized || normalized.length > PROJECT_MAX_TAG_LENGTH) {
      return { ok: false, reason: `标签「${tag}」非法（非空且 ≤ ${PROJECT_MAX_TAG_LENGTH} 字符）` };
    }
  }
  return { ok: true };
}

export function validateChildInput(input: {
  type: string;
  title: string;
  summary: string;
}): { ok: boolean; reason?: string } {
  if (!CHILD_AFFAIR_TYPES.includes(input.type as ChildAffairType)) {
    return { ok: false, reason: `未知子事务类型 ${input.type}（MVP：${CHILD_AFFAIR_TYPES.join('/')}）` };
  }
  return validateProjectInput({ title: input.title, summary: input.summary, tags: [] });
}

export function validateCommentText(text: string): { ok: boolean; reason?: string } {
  const normalized = normalizeProjectText(text);
  if (!normalized) {
    return { ok: false, reason: '发言内容不能为空' };
  }
  if (normalized.length > PROJECT_MAX_COMMENT_LENGTH) {
    return { ok: false, reason: `发言过长（上限 ${PROJECT_MAX_COMMENT_LENGTH} 字符）` };
  }
  return { ok: true };
}

export function validateDocInput(input: { title: string; body: string }): { ok: boolean; reason?: string } {
  const title = normalizeProjectText(input.title);
  if (!title) {
    return { ok: false, reason: '文档标题不能为空' };
  }
  if (title.length > PROJECT_MAX_DOC_TITLE_LENGTH) {
    return { ok: false, reason: `文档标题过长（上限 ${PROJECT_MAX_DOC_TITLE_LENGTH} 字符）` };
  }
  if (input.body.length > PROJECT_MAX_DOC_BODY_LENGTH) {
    return { ok: false, reason: `文档正文过长（上限 ${PROJECT_MAX_DOC_BODY_LENGTH} 字符）` };
  }
  return { ok: true };
}

// ------------------------------------------------------------------
// 规则文档（「维护者制」模板，§2/§7 MVP 唯一模板）
// ------------------------------------------------------------------

/**
 * 创世规则文档（维护者制模板）：
 * - engine b1 + pubPeriod/ruleChange 内核可判定字段与 spark-affairs 同构
 *   （内核静态检查要求）；
 * - closeConditions = 处置关闭条件（R1：op-count × project.disposition × 1，
 *   内核可判定形态）——pubPeriod 因此是活字段：bug/proposal 处置决议按其
 *   走公示期（决议载荷 pubPeriod.delayMs 须与规则版本逐字一致，内核复算
 *   比对 PubPeriodMismatch 即拒）；ruleChange 走 delayed-veto 机制（规则
 *   修改通道，与关闭条件无涉）；
 * - **maintainers 顶层字段**：写权集合，登记各维护者的插件域身份 id
 *   （decisions 实施期补录口径；spark-kanban/spark-git-repo 读侧同取
 *   rules.maintainers，跨插件互操作依赖此约定）；
 * - sparkProject 段为插件语义参数（内核不解释，随 affairId/rulesHash 被承诺）：
 *   PR 治理写明「单维护者合并回执即生效 + 禁止非快进写回」（档一-3）。
 */
export function buildProjectRulesDoc(maintainers: string[]): Record<string, unknown> {
  return {
    engine: 'b1',
    closeConditions: [buildDispositionCloseCondition()],
    pubPeriod: { delayMs: PROJECT_PUB_PERIOD_MS, vetoThreshold: { count: 1 } },
    ruleChange: { kind: 'delayed-veto', delayMs: PROJECT_PUB_PERIOD_MS, vetoThreshold: { count: 1 } },
    exec: null,
    maintainers: [...new Set(maintainers)],
    sparkProject: {
      template: PROJECT_RULES_TEMPLATE,
      childTypes: [...CHILD_AFFAIR_TYPES],
      prGovernance: 'single-maintainer-merge-receipt',
      fastForwardOnly: true,
      publicPublishDeclared: true
    }
  };
}

/**
 * 子事务创世规则文档：形状与项目议题同构（内核静态检查同一套）；
 * maintainers 快照自 parent 项目议题创世时刻的写权集合——**权威源仍是
 * parent 的现行规则文档**（读侧/写侧都经 parent 重取，见 service 层），
 * 此处快照只为无 parent 上下文时的 fail-closed 兜底可读性。
 */
export function buildChildRulesDoc(maintainersSnapshot: string[]): Record<string, unknown> {
  return {
    engine: 'b1',
    closeConditions: [buildDispositionCloseCondition()],
    pubPeriod: { delayMs: PROJECT_PUB_PERIOD_MS, vetoThreshold: { count: 1 } },
    ruleChange: { kind: 'delayed-veto', delayMs: PROJECT_PUB_PERIOD_MS, vetoThreshold: { count: 1 } },
    exec: null,
    maintainers: [...new Set(maintainersSnapshot)],
    sparkProject: { childAffair: true, maintainerAuthority: 'parent' }
  };
}

/**
 * 写权集合提取（顶层 rules.maintainers；与 spark-kanban extractWriteSet /
 * spark-git-repo 同口径）。规则不可读/未声明 → 空集 = fail-closed
 * （无人的处置/状态操作被采纳，诚实降级）。
 */
export function extractMaintainers(rulesDoc: unknown): string[] {
  if (typeof rulesDoc !== 'object' || rulesDoc === null || Array.isArray(rulesDoc)) {
    return [];
  }
  const raw = (rulesDoc as Record<string, unknown>).maintainers;
  if (!Array.isArray(raw)) {
    return [];
  }
  return [...new Set(raw.filter((item): item is string => typeof item === 'string' && /^[0-9a-f]{64}$/.test(item)))];
}

// ------------------------------------------------------------------
// 创世记录读侧
// ------------------------------------------------------------------

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** 创世记录 → 元数据投影；结构不符返回 null（创世未同步/形状非法，不编造） */
export function readGenesisMeta(
  affairId: string,
  genesis: unknown,
  extras: { following: boolean; operationCount: number }
): ProjectMeta | null {
  const record = asRecord(genesis);
  if (!record || typeof record.title !== 'string') {
    return null;
  }
  return {
    affairId,
    type: typeof record.type === 'string' ? record.type : '',
    title: record.title,
    summary: typeof record.summary === 'string' ? record.summary : '',
    tags: Array.isArray(record.tags) ? record.tags.filter((tag): tag is string => typeof tag === 'string') : [],
    isPublic: record.publish === true,
    createdAt: typeof record.createdAt === 'number' ? record.createdAt : 0,
    following: extras.following,
    operationCount: extras.operationCount
  };
}

/** 创世 refs 是否含 {target: projectAffairId, rel: 'parent'}（§10 枚举） */
export function isChildOfProject(genesis: unknown, projectAffairId: string): boolean {
  const record = asRecord(genesis);
  const refs = record?.refs;
  if (!Array.isArray(refs)) {
    return false;
  }
  return refs.some((ref) => {
    const entry = asRecord(ref);
    return entry?.rel === 'parent' && entry?.target === projectAffairId;
  });
}

/** 从创世记录取 parent 项目议题 affairId（无 parent → null） */
export function parentAffairOf(genesis: unknown): string | null {
  const record = asRecord(genesis);
  const refs = record?.refs;
  if (!Array.isArray(refs)) {
    return null;
  }
  for (const ref of refs) {
    const entry = asRecord(ref);
    if (entry?.rel === 'parent' && typeof entry.target === 'string') {
      return entry.target;
    }
  }
  return null;
}

// ------------------------------------------------------------------
// 子事务处置推导（因果序 + opHash tie-break + 写权过滤；红线：declaredAt
// 永不参与——与 spark-kanban deriveStatusKeyFromLog 同一算法骨架）
// ------------------------------------------------------------------

/** 处置推导形态：receipt = PR 合并回执（content 动议即生效，档一-3）；resolution = bug/proposal/通用（决议操作入公示期，R1） */
export type DispositionMode = 'receipt' | 'resolution';

/**
 * 从子事务操作日志推导处置状态：最新有效处置操作（actor ∈ writeSet）。
 * - receipt 形态采纳 content + project.disposition 动议（PR 回执）；
 * - resolution 形态采纳 opType=resolution 且 payload.result ∈ adopted/closed
 *   的决议操作（处置动议 content op 只是计入关闭判定的证据，不单独生效——
 *   诚实边界：决议未入日志前状态保持「开放」）；note 从 countedOps 引用的
 *   动议操作恢复；
 * - 写权集合外/畸形操作留痕日志但不参与推导（append-only 挡不住自制客户端
 *   伪造，读侧必须自证）。
 */
export function deriveDisposition(
  ops: Array<{ opHash: string; op: Record<string, unknown> }>,
  writeSet: ReadonlySet<string>,
  mode: DispositionMode
): DispositionView {
  const prevOf = new Map<string, string>();
  for (const entry of ops) {
    const prev = entry.op?.prevOpHash;
    if (typeof prev === 'string') {
      prevOf.set(entry.opHash, prev);
    }
  }

  const candidates: Array<{ opHash: string; action: DispositionAction; note: string | null; actor: string }> = [];
  for (const entry of ops) {
    const op = entry.op;
    const payload = asRecord(op.payload);
    const actor = asRecord(op.actor)?.identity;
    if (typeof actor !== 'string' || !writeSet.has(actor)) {
      continue;
    }
    if (mode === 'receipt') {
      if (op.opType !== 'content' || !payload || payload.kind !== DISPOSITION_KIND) {
        continue;
      }
      const action = payload.action;
      if (action !== 'adopted' && action !== 'closed') {
        continue;
      }
      candidates.push({
        opHash: entry.opHash,
        action,
        note: typeof payload.note === 'string' && payload.note.trim() ? payload.note : null,
        actor
      });
      continue;
    }
    // resolution 形态：决议操作（affair.md §6.1 线形）
    if (op.opType !== 'resolution' || !payload) {
      continue;
    }
    const result = payload.result;
    if (result !== 'adopted' && result !== 'closed') {
      continue;
    }
    // note 从 countedOps 引用的处置动议恢复（决议载荷本身不复制动议正文）
    let note: string | null = null;
    const counted = Array.isArray(payload.countedOps) ? payload.countedOps : [];
    const motion = ops.find(
      (candidate) => counted.includes(candidate.opHash) && asRecord(candidate.op.payload)?.kind === DISPOSITION_KIND
    );
    if (motion) {
      const motionPayload = asRecord(motion.op.payload);
      if (typeof motionPayload?.note === 'string' && motionPayload.note.trim()) {
        note = motionPayload.note;
      }
    }
    candidates.push({ opHash: entry.opHash, action: result, note, actor });
  }
  if (candidates.length === 0) {
    return { state: 'open', opHash: null, note: null, actorIdentity: null };
  }

  // 祖先闭包（visited 防环；prev 指向创世/未知操作止步，同内核口径）
  const ancestorClosure = (root: string): Set<string> => {
    const visited = new Set<string>();
    const stack = [root];
    while (stack.length > 0) {
      const hash = stack.pop() as string;
      if (!visited.add(hash)) {
        continue;
      }
      const prev = prevOf.get(hash);
      if (prev !== undefined && prevOf.has(prev)) {
        stack.push(prev);
      }
    }
    return visited;
  };

  const candidateHashes = new Set(candidates.map((candidate) => candidate.opHash));
  const scored = candidates.map((candidate) => {
    const closure = ancestorClosure(candidate.opHash);
    let score = 0;
    for (const other of closure) {
      if (other !== candidate.opHash && candidateHashes.has(other)) {
        score += 1;
      }
    }
    return { ...candidate, score };
  });
  scored.sort((a, b) => b.score - a.score || (a.opHash < b.opHash ? 1 : a.opHash > b.opHash ? -1 : 0));
  const latest = scored[0];
  return { state: latest.action, opHash: latest.opHash, note: latest.note, actorIdentity: latest.actor };
}

/** 决议状态集合 → 徽标（公示期内 pending/unanchored 显示「待确认」，不美化） */
export function resolutionBadgeFromStates(
  states: Array<'pending' | 'effective' | 'vetoed' | 'unanchored'>
): ChildAffairView['resolutionBadge'] {
  if (states.some((state) => state === 'effective')) {
    return 'effective';
  }
  if (states.some((state) => state === 'pending' || state === 'unanchored')) {
    return 'pending';
  }
  if (states.length > 0 && states.every((state) => state === 'vetoed')) {
    return 'vetoed';
  }
  return 'open';
}

export const RESOLUTION_BADGE_LABELS: Record<ChildAffairView['resolutionBadge'], string> = {
  open: '开放',
  pending: '待确认',
  effective: '已生效',
  vetoed: '已否决'
};

// ------------------------------------------------------------------
// 时间线（讨论流：发言/子事务通告/处置/决议同流，§4）
// ------------------------------------------------------------------

/** 操作 → 时间线条目（未识别类型归 other 原样呈现 opType/kind） */
export function toTimelineEntry(opHash: string, op: Record<string, unknown>): TimelineEntry {
  const payload = asRecord(op.payload);
  const kind = typeof payload?.kind === 'string' ? payload.kind : '';
  const actor = asRecord(op.actor)?.identity;
  const base = {
    opHash,
    actorIdentity: typeof actor === 'string' ? actor : null,
    declaredAt: typeof op.declaredAt === 'number' ? op.declaredAt : 0
  };
  if (op.opType === 'resolution') {
    const result = typeof payload?.result === 'string' ? payload.result : '';
    const label = result === 'adopted' || result === 'closed' ? DISPOSITION_LABELS[result] : '';
    return {
      ...base,
      kind: 'resolution',
      text: label ? `处置决议：${label}（公示期状态以决议徽标为准）` : '决议操作（公示期状态以决议区为准）'
    };
  }
  if (op.opType === 'content' && kind === PROJECT_COMMENT_KIND) {
    return { ...base, kind: 'comment', text: typeof payload?.text === 'string' ? payload.text : '' };
  }
  if (op.opType === 'content' && kind === CHILD_NOTICE_KIND) {
    const childType = typeof payload?.childType === 'string' ? payload.childType : '';
    const title = typeof payload?.title === 'string' ? payload.title : '';
    const childId = typeof payload?.childAffairId === 'string' ? payload.childAffairId.slice(0, 12) : '';
    return {
      ...base,
      kind: 'child-notice',
      text: `子事务通告：[${CHILD_TYPE_LABELS[childType] ?? childType}] ${title}（${childId}…）`
    };
  }
  if (op.opType === 'content' && kind === DISPOSITION_KIND) {
    const action = payload?.action === 'adopted' || payload?.action === 'closed' ? payload.action : undefined;
    const note = typeof payload?.note === 'string' ? payload.note : '';
    return {
      ...base,
      kind: 'disposition',
      text: `处置操作：${action ? DISPOSITION_LABELS[action] : '未知动作'}${note ? `——${note}` : ''}`,
      ...(action ? { action } : {})
    };
  }
  return { ...base, kind: 'other', text: `操作 ${String(op.opType ?? '?')}${kind ? `/${kind}` : ''}` };
}

/** 展示序：声明时刻 + opHash tie-break（仅呈现，不参与任何判定） */
export function sortTimeline(entries: TimelineEntry[]): TimelineEntry[] {
  return [...entries].sort(
    (a, b) => a.declaredAt - b.declaredAt || (a.opHash < b.opHash ? -1 : a.opHash > b.opHash ? 1 : 0)
  );
}

// ------------------------------------------------------------------
// 项目文档（append-only 版本链，§3.2）
// ------------------------------------------------------------------

/** 文档版本折叠：同一 docId 取最大 seq 为当前版本（createdAt+docId tie-break 稳定） */
export function deriveDocSummaries(versions: ProjectDocVersion[]): ProjectDocSummary[] {
  const byDoc = new Map<string, ProjectDocVersion[]>();
  for (const version of versions) {
    const list = byDoc.get(version.docId) ?? [];
    list.push(version);
    byDoc.set(version.docId, list);
  }
  const summaries: ProjectDocSummary[] = [];
  for (const [docId, list] of byDoc) {
    const ordered = [...list].sort(
      (a, b) => a.seq - b.seq || a.createdAt - b.createdAt
    );
    const latest = ordered[ordered.length - 1];
    summaries.push({
      docId,
      title: latest.title,
      latestSeq: latest.seq,
      updatedAt: latest.createdAt,
      versionCount: ordered.length
    });
  }
  return summaries.sort((a, b) => b.updatedAt - a.updatedAt || (a.docId < b.docId ? -1 : 1));
}

/** 单文档版本链（seq 升序原样返回，历史可溯） */
export function docHistory(versions: ProjectDocVersion[], docId: string): ProjectDocVersion[] {
  return versions
    .filter((version) => version.docId === docId)
    .sort((a, b) => a.seq - b.seq || a.createdAt - b.createdAt);
}
