/**
 * 项目（议题）插件（spark-project）· 业务服务层（project.md v0.2 + 拍板口径）。
 *
 * 职责边界：
 * - 事务层全部走 sdk.affairs：项目议题/子事务创建（sdk.affairs.create，含
 *   档二-2 publish 声明位）、关注/取关、讨论/通告/处置提交（submitOp，
 *   签名入日志）、决议/规则/阶梯/公开履历读取（内核确定性推导原样呈现）；
 *   处置双形态（R1 选 a）：bug/proposal/通用 = 处置动议（content）+ 决议
 *   操作（resolution）入内核公示期；PR = 单维护者合并回执即生效（档一-3）；
 * - 插件数据层走 sdk.data（集合名前缀 == 本插件 id）：docs（项目文档
 *   append-only 版本链）、workspace（工作区配置 lww）、drafts（本地草稿
 *   local）、notified（通知去重台账 local——iframe opaque origin 恒无
 *   localStorage，台账一律走 sdk.data，插件群统一口径）；
 *   **草稿恢复 UI 未接**（S4 如实标注：saveDraft/listDrafts/deleteDraft
 *   服务层就绪并被测，视图层「从草稿继续编辑」入口排后续迭代）；
 * - **库依赖组合（档二-3，plugin-types 库包形态）**：KanbanService /
 *   ReleaseManagerService 来自 vendor/ 锚定的库包源码，以本插件 id 作
 *   namespace 构造（`new KanbanService(sdk, 'spark-project')`）——内核
 *   plugindata 强制集合名前缀 == 调用方插件 id，数据全部写本插件命名
 *   空间，库无自己的数据域；sdk.docs 集合由内核按调用方域隔离（发布
 *   管理数据自动落到本插件域）；
 * - 代码仓库件（spark-git-repo）**缺库包形态**（无 lib.ts、服务无
 *   namespace 参数化、集合名硬编码 spark-git-repo:*）——本插件不组合它，
 *   代码视图诚实降级「未启用」（缺口记录随交付报告上报，并行任务补齐后
 *   再接入）；
 * - 版本卡片唯一推送源是发布管理件（档一-2）：本插件不自推版本卡片；
 *   组合形态下 release-manager 推送的卡片以本插件 manifest 的
 *   'release-card' 视图渲染；
 * - 通知降级口径（档二-4）：background 视图线形未确认，MVP = 插件加载时
 *   补发 + 节流（本地台账去重，权限/限流被拒即中止本轮不轰炸）；
 * - 移动端整插件只读（档三-2）：视图层禁用全部写入口并如实标注；
 * - 能力缺失一律降级不报错：无 sdk.affairs → 整个事务面显示「未启用」。
 *
 * 签名主体诚实口径（同 spark-affairs）：事务操作 actor 为本插件域身份，
 * 不代表操作者个人身份；maintainers 登记的也是插件域身份 id（补录口径）。
 */
import type {
  AffairChangeEvent,
  AffairGenesisInput,
  AffairLogEntry,
  AffairOpStatus,
  AffairPublicProfile,
  PluginAffairsAPI,
  PluginSDK
} from '../../packages/plugin-sdk/src';
import {
  CHILD_TYPE_LABELS,
  DISPOSITION_LABELS,
  PROJECT_AFFAIR_TYPE,
  PROJECT_PUB_PERIOD_MS,
  buildDispositionCloseCondition,
  deriveDisposition,
  deriveDocSummaries,
  docHistory,
  extractMaintainers,
  isChildOfProject,
  isValidAffairId,
  normalizeProjectText,
  readGenesisMeta,
  resolutionBadgeFromStates,
  sortTimeline,
  toTimelineEntry,
  validateChildInput,
  validateCommentText,
  validateDocInput,
  validateProjectInput,
  type ChildAffairType,
  type ChildAffairView,
  type DispositionAction,
  type DispositionMode,
  type ProjectDocSummary,
  type ProjectDocVersion,
  type ProjectMemberView,
  type ProjectMeta,
  type TimelineEntry
} from './model';
import {
  buildChildGenesisInput,
  buildChildNoticePayload,
  buildCommentPayload,
  buildDispositionPayload,
  buildOpDraft,
  buildProjectGenesisInput,
  buildResolutionDraft,
  deriveIdentity,
  signPayload,
  type AffairActor
} from './wire';
// 库依赖组合（档二-3 vendor 锚定；namespace = 本插件 id，数据写组合者命名空间）
import { KanbanService } from './vendor/github.com/welyin/spark/plugins/spark-kanban/service';
import {
  buildBoardView,
  type KanbanBoard,
  type KanbanColumnView
} from './vendor/github.com/welyin/spark/plugins/spark-kanban/model';
import { ReleaseManagerService } from './vendor/github.com/welyin/spark/plugins/spark-release-manager/service';
import {
  deriveReleaseState,
  filterAuthorizedReleaseEvents,
  releaseEventOperatorSet,
  RELEASE_STATE_LABELS,
  type ReleaseEvent,
  type ReleaseManagerConfig,
  type ReleaseRecord,
  type ReleaseState
} from './vendor/github.com/welyin/spark/plugins/spark-release-manager/model';

/** 本插件用到的 sdk.affairs 方法核对清单（fail-fast 点名缺失，同 spark-feedback 纪律） */
export const REQUIRED_AFFAIRS_METHODS = [
  'create', 'follow', 'unfollow', 'listFollowed', 'readLog', 'submitOp',
  'readResolution', 'readRules', 'ladderStatus', 'publicProfile', 'onChange'
] as const;

export const AFFAIRS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.affairs（事务模块）——项目议题的创建/讨论/子事务均不可用，本插件只能显示本提示。';

/** 集合名（内核 plugindata 强制前缀 == 调用方插件 id） */
const DOCS_COLLECTION = 'spark-project:docs';
const WORKSPACE_COLLECTION = 'spark-project:workspace';
const DRAFTS_COLLECTION = 'spark-project:drafts';
const NOTIFIED_COLLECTION = 'spark-project:notified';

function hasModule<T extends object>(mod: Partial<T> | undefined, methods: readonly string[]): mod is T {
  return Boolean(mod && methods.every((method) => typeof (mod as Record<string, unknown>)[method] === 'function'));
}

function nonNull<T>(value: T | null): value is T {
  return value !== null;
}

/** 记录 id：时间戳 + 单调计数 + 随机后缀（同 spark-kanban 口径） */
let idCounter = 0;
function newId(prefix: string): string {
  idCounter = (idCounter + 1) & 0xffffff;
  return `${prefix}_${Date.now()}_${idCounter.toString(16).padStart(6, '0')}_${Math.random().toString(16).slice(2, 10)}`;
}

/** 看板初始化结果（组织空间非管理员建板被拒时如实上报原因） */
export type BoardInitResult =
  | { ok: true; board: KanbanBoard }
  | { ok: false; reason: string };

/** 发布区读侧投影（视图层不直接碰库件类型细节） */
export type ReleaseRow = {
  release: ReleaseRecord;
  state: ReleaseState;
  stateLabel: string;
};

export class ProjectService {
  private readonly affairs: PluginAffairsAPI | null;
  private collectionsReady: Promise<void> | null = null;
  private actor: AffairActor | null = null;
  private kanban: KanbanService | null = null;
  private releases: ReleaseManagerService | null = null;

  constructor(private readonly sdk: PluginSDK) {
    this.affairs = hasModule<PluginAffairsAPI>(sdk.affairs, REQUIRED_AFFAIRS_METHODS) ? sdk.affairs : null;
  }

  /** 事务模块可用性（视图层据此把事务面整体降级为提示页） */
  get affairsAvailable(): boolean {
    return this.affairs !== null;
  }

  /** 本机操作者身份（本插件域身份 id；尚无写操作时为 null） */
  get viewerIdentity(): string | null {
    return this.actor?.identity ?? null;
  }

  private requireAffairs(): PluginAffairsAPI {
    if (!this.affairs) {
      throw new Error(AFFAIRS_MODULE_MISSING);
    }
    return this.affairs;
  }

  // ------------------------------------------------------------------
  // 集合声明与读写（sdk.data；写入前必须声明，声明幂等）
  // ------------------------------------------------------------------

  private ensureCollections(): Promise<void> {
    this.collectionsReady ??= (async () => {
      await this.sdk.data.declareCollection({ name: DOCS_COLLECTION, merge: 'append-only', scope: 'sync' });
      await this.sdk.data.declareCollection({ name: WORKSPACE_COLLECTION, merge: 'lww-record', scope: 'sync' });
      await this.sdk.data.declareCollection({ name: DRAFTS_COLLECTION, merge: 'lww-record', scope: 'local' });
      await this.sdk.data.declareCollection({ name: NOTIFIED_COLLECTION, merge: 'lww-record', scope: 'local' });
    })();
    return this.collectionsReady;
  }

  /** 远端合入本插件集合时的回调（视图层借此重载收敛） */
  async subscribeDataChanges(handler: () => void): Promise<void> {
    if (typeof this.sdk.data.onChange !== 'function') {
      return;
    }
    await this.sdk.data.onChange(handler);
  }

  // ------------------------------------------------------------------
  // 域身份与签名（identity:sign；签名是内核入站硬要求，拒绝即失败不降级）
  // ------------------------------------------------------------------

  private async ensureActor(): Promise<AffairActor> {
    if (this.actor) {
      return this.actor;
    }
    const probe = await this.sdk.identity.sign('spark-project:actor-probe');
    this.actor = {
      kind: 'person',
      identity: deriveIdentity(probe.publicKey),
      publicKey: probe.publicKey
    };
    return this.actor;
  }

  private async signRecord(record: Record<string, unknown>): Promise<Record<string, unknown>> {
    const signed = await this.sdk.identity.sign(signPayload(record));
    return { ...record, sig: signed.signature };
  }

  /** 订阅事务副本变更（变更通知非可靠队列，handler 内重读收敛） */
  subscribeAffairChanges(handler: (event: AffairChangeEvent) => void): Promise<void> {
    return this.requireAffairs().onChange(handler);
  }

  private async submitContentOp(
    affairId: string,
    payload: Parameters<typeof buildOpDraft>[2]
  ): Promise<{ opHash: string; status: AffairOpStatus }> {
    const affairs = this.requireAffairs();
    const actor = await this.ensureActor();
    const log = await affairs.readLog(affairId);
    const draft = buildOpDraft(affairId, actor, payload, log.heads, Date.now());
    const result = await affairs.submitOp(await this.signRecord(draft));
    return { opHash: result.opHash, status: result.status };
  }

  // ------------------------------------------------------------------
  // 项目议题：创建 / 关注 / 列表
  // ------------------------------------------------------------------

  /**
   * 创建项目议题（affairs:write）：
   * 1. 输入校验；2. **公开发布纵深确认**（档二-2 补录：publish:true 将
   *    标题/简介/标签洪泛进全网公共目录，隐私语义升级——视图层显式确认
   *    之外，服务层 fail-closed 复核 confirmedPublish===true，不依赖
   *    视图层纪律）；3. sdk.affairs.create（创世含维护者制规则文档，
   *    maintainers 初始 = 本插件域身份 id）；4. 议题说明作为首条发言
   *    入日志（关注者副本时间线不为空）；5. 创建回执卡片。
   * 后两步是呈现便利：失败降格为部分成功态返回（commentPosted/cardSent=false），
   * 议题本体已成立，不抛错回滚（S2）。
   */
  async createProject(
    input: { title: string; summary: string; tags: string[]; publish: boolean },
    options: { confirmedPublish?: boolean } = {}
  ): Promise<{ affairId: string; commentPosted: boolean; cardSent: boolean }> {
    const verdict = validateProjectInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    if (input.publish === true && options.confirmedPublish !== true) {
      throw new Error(
        '公开发布会把项目标题/简介/标签洪泛进全网公共目录（任何 indexer 可收录可搜，不可撤回）——请显式确认后再创建'
      );
    }
    const affairs = this.requireAffairs();
    const actor = await this.ensureActor();
    const genesisInput: AffairGenesisInput = buildProjectGenesisInput(input, actor.identity);
    const { affairId } = await affairs.create(genesisInput);
    let commentPosted = false;
    try {
      await this.submitContentOp(affairId, buildCommentPayload(normalizeProjectText(input.summary)));
      commentPosted = true;
    } catch (error) {
      console.warn('[spark-project] 首条议题说明写入失败（议题本体已成立，不阻断）：', error);
    }
    const cardSent = await this.notifyProjectCard({
      kind: 'project-created',
      affairId,
      title: normalizeProjectText(input.title)
    });
    return { affairId, commentPosted, cardSent };
  }

  /** 关注已有议题（粘贴创世记录原文；内核全链校验 + affairId 自认证复算） */
  async followGenesis(genesis: unknown): Promise<string> {
    if (typeof genesis !== 'object' || genesis === null || Array.isArray(genesis)) {
      throw new Error('创世记录必须是 JSON 对象');
    }
    return this.requireAffairs().follow(genesis as Record<string, unknown>);
  }

  /** 取关（只删关注簿记，保留已复制数据；关注即副本语义如实告知由视图层负责） */
  unfollow(affairId: string): Promise<void> {
    return this.requireAffairs().unfollow(affairId);
  }

  /** 本机关注的项目议题列表（type=project；创世未同步的跳过不编造） */
  async listProjects(): Promise<ProjectMeta[]> {
    const affairs = this.requireAffairs();
    const ids = await affairs.listFollowed();
    const items = await Promise.all(
      ids.map(async (affairId) => {
        const log = await affairs.readLog(affairId);
        const meta = readGenesisMeta(affairId, log.genesis, {
          following: log.followedAt !== null,
          operationCount: log.ops.length
        });
        if (!meta || meta.type !== PROJECT_AFFAIR_TYPE) {
          return null;
        }
        return meta;
      })
    );
    return items.filter(nonNull).sort((a, b) => b.createdAt - a.createdAt || (a.affairId < b.affairId ? -1 : 1));
  }

  /** 项目详情（元数据 + 规则文档版本链 + 维护者集合） */
  async getProject(affairId: string): Promise<{
    meta: ProjectMeta;
    maintainers: string[];
    rulesVersions: number;
  } | null> {
    const affairs = this.requireAffairs();
    const log = await affairs.readLog(affairId);
    const meta = readGenesisMeta(affairId, log.genesis, {
      following: log.followedAt !== null,
      operationCount: log.ops.length
    });
    if (!meta) {
      return null;
    }
    let maintainers: string[] = [];
    let rulesVersions = 0;
    try {
      const rulesView = await affairs.readRules(affairId);
      maintainers = extractMaintainers(rulesView.current.rules);
      rulesVersions = rulesView.versions.length;
    } catch {
      // 规则不可读 → 空写权集合（fail-closed，与 kanban/git-repo 同口径）
    }
    return { meta, maintainers, rulesVersions };
  }

  // ------------------------------------------------------------------
  // 讨论流（操作日志时间线，§4）
  // ------------------------------------------------------------------

  /** 操作时间线（展示序 = 声明时刻 + opHash；判定序纪律见 model 头注） */
  async listTimeline(affairId: string): Promise<TimelineEntry[]> {
    const log = await this.requireAffairs().readLog(affairId);
    return sortTimeline(log.ops.map((entry: AffairLogEntry) => toTimelineEntry(entry.opHash, entry.op)));
  }

  /** 发言（观察层零门槛；垃圾评论靠客户端过滤与主持折叠，不进协议） */
  async submitComment(affairId: string, text: string): Promise<{ opHash: string; status: AffairOpStatus }> {
    const verdict = validateCommentText(text);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    return this.submitContentOp(affairId, buildCommentPayload(text));
  }

  // ------------------------------------------------------------------
  // 子事务（bug/建议/PR；refs rel=parent 父子引用，§3.4）
  // ------------------------------------------------------------------

  /**
   * 聚合项目子事务（自动聚合主路径 = 反馈回流零成本落进列表）：
   * 本机关注的事务中创世 refs 含 {target: 项目 affairId, rel: 'parent'} 者；
   * 处置状态按 parent 现行规则写权集合过滤推导（读侧自证），决议徽标
   * 原样呈现公示期状态。创世未同步到位的跳过而非编造占位。
   */
  async listChildren(projectAffairId: string): Promise<ChildAffairView[]> {
    const affairs = this.requireAffairs();
    // 写权集合取自 parent 项目议题现行规则文档（kanban/git-repo 同口径）
    let writeSet: ReadonlySet<string> = new Set();
    try {
      const rulesView = await affairs.readRules(projectAffairId);
      writeSet = new Set(extractMaintainers(rulesView.current.rules));
    } catch {
      // 规则不可读 → 空集 = 无人的处置操作被采纳（诚实降级为全部开放）
    }
    const followed = await affairs.listFollowed();
    const children: ChildAffairView[] = [];
    for (const affairId of followed) {
      if (affairId === projectAffairId) {
        continue;
      }
      const log = await affairs.readLog(affairId);
      if (!log.genesis || !isChildOfProject(log.genesis, projectAffairId)) {
        continue;
      }
      const meta = readGenesisMeta(affairId, log.genesis, { following: true, operationCount: log.ops.length });
      if (!meta) {
        continue;
      }
      const resolutions = await affairs.readResolution(affairId);
      // 决议徽标同样按写权集合过滤：内核 replay 只复算关闭条件满足性（不识别
      // 「谁是维护者」），readResolution 视图不携带 actor——回查本机日志
      // opHash→actor，集合外签名者提交的决议不进徽标（读侧自证，同一约定）
      const actorByOpHash = new Map<string, string>();
      for (const entry of log.ops) {
        const identity = (entry.op?.actor as Record<string, unknown> | undefined)?.identity;
        if (typeof identity === 'string') {
          actorByOpHash.set(entry.opHash, identity);
        }
      }
      const authorizedStates = resolutions.resolutions
        .filter((item) => writeSet.has(actorByOpHash.get(item.opHash) ?? ''))
        .map((item) => item.state);
      // PR = 合并回执即生效（无内核决议）；bug/proposal/通用 = 决议操作入公示期（R1）
      const mode: DispositionMode = meta.type === 'pr' ? 'receipt' : 'resolution';
      const disposition = deriveDisposition(log.ops, writeSet, mode);
      children.push({
        affairId,
        type: meta.type,
        typeLabel: CHILD_TYPE_LABELS[meta.type] ?? `通用子事务（${meta.type || '未知类型'}）`,
        title: meta.title,
        summary: meta.summary,
        createdAt: meta.createdAt,
        disposition,
        resolutionBadge:
          mode === 'receipt'
            ? disposition.state === 'open'
              ? 'open'
              : 'effective'
            : resolutionBadgeFromStates(authorizedStates)
      });
    }
    return children.sort((a, b) => a.createdAt - b.createdAt || (a.affairId < b.affairId ? -1 : 1));
  }

  /**
   * 发起子事务（affairs:write；反馈回流同一路径，档一-1）：项目议题须
   * 本机已关注且创世可读；rules 快照自 parent 现行 maintainers；PR 可
   * 携带 bundle 内容面 cid（extra.pr，随 affairId 被承诺）。创建成功后
   * 在项目议题日志追加子事务通告（§3.1 操作日志含子事务创建通告；
   * 通告失败不阻断——子事务本体已成立，通告只是呈现便利）。
   */
  async createChild(
    projectAffairId: string,
    input: { type: ChildAffairType; title: string; summary: string; bundleCid?: string }
  ): Promise<{ affairId: string; noticePosted: boolean }> {
    const verdict = validateChildInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    if (input.bundleCid !== undefined && input.bundleCid.trim() !== '' && !isValidAffairId(input.bundleCid.trim())) {
      throw new Error('PR bundle 的内容面 cid 必须是 64 位小写 hex（SHA-256）');
    }
    const affairs = this.requireAffairs();
    const followed = await affairs.listFollowed();
    if (!followed.includes(projectAffairId)) {
      throw new Error('尚未关注目标项目议题——请先关注后再发起子事务（关注即副本语义）');
    }
    const log = await affairs.readLog(projectAffairId);
    if (!log.genesis) {
      throw new Error('项目议题创世记录尚未同步到本机（等待复制收敛），fail-closed 中止');
    }
    let maintainers: string[] = [];
    try {
      const rulesView = await affairs.readRules(projectAffairId);
      maintainers = extractMaintainers(rulesView.current.rules);
    } catch {
      // 规则不可读：子事务仍允许创建（创世只快照空集合；处置权威源在 parent，
      // 读侧会按 parent 现行规则重取，不因此被锁死）
    }
    const genesisInput = buildChildGenesisInput(projectAffairId, input, maintainers);
    const { affairId } = await affairs.create(genesisInput);
    let noticePosted = false;
    try {
      await this.submitContentOp(
        projectAffairId,
        buildChildNoticePayload({ childAffairId: affairId, childType: input.type, title: normalizeProjectText(input.title) })
      );
      noticePosted = true;
    } catch (error) {
      console.warn('[spark-project] 子事务通告写入失败（子事务本体已成立，不阻断）：', error);
    }
    return { affairId, noticePosted };
  }

  /**
   * 维护者处置（采纳/关闭）：
   * - 写权校验 fail-closed——parent 项目议题现行规则 rules.maintainers ∌
   *   当前插件域身份即拒（内核侧门槛算术把关，本层是客户端前置拦截）；
   * - 归属校验 fail-closed（S1）：子事务创世须含 refs rel=parent 指向本项目
   *   议题，防止把处置操作写进不相干事务的日志（append-only 不可收回）；
   * - 第一步两形态共通：处置动议 = 维护者签名内容操作（project.disposition）
   *   入子事务日志；**PR 就此完成**（档一-3 单维护者合并回执即生效）；
   * - bug/proposal/通用（R1 选 a）：第二步提交决议操作（opType=resolution，
   *   condition 逐字回引子事务规则 closeConditions，countedOps=[动议 opHash]，
   *   rulesHash/pubPeriod 取子事务现行规则版本）——入内核公示期，公示期内
   *   决议徽标「待确认」，期满无阈值异议转「已生效」；
   * - 动议未被内核接受（pending=未知指向暂存等）时不续投决议（复算必败），
   *   如实返回 resolutionOpHash=null。
   */
  async submitDisposition(
    projectAffairId: string,
    childAffairId: string,
    action: DispositionAction,
    note?: string
  ): Promise<{ opHash: string; status: AffairOpStatus; resolutionOpHash: string | null }> {
    const affairs = this.requireAffairs();
    const actor = await this.ensureActor();
    let writeSet: ReadonlySet<string>;
    try {
      const rulesView = await affairs.readRules(projectAffairId);
      writeSet = new Set(extractMaintainers(rulesView.current.rules));
    } catch (error) {
      throw new Error(`无法读取项目议题规则文档，处置写权校验 fail-closed 中止：${(error as Error).message}`);
    }
    if (!writeSet.has(actor.identity)) {
      const empty = writeSet.size === 0 ? '（规则文档未声明 maintainers，无人可处置）' : '';
      throw new Error(
        `处置被拒绝：当前身份 ${actor.identity.slice(0, 12)}… 不在项目议题规则声明的维护者集合（rules.maintainers）中${empty}`
      );
    }
    const childLog = await affairs.readLog(childAffairId);
    if (!childLog.genesis) {
      throw new Error('子事务创世记录尚未同步到本机（等待复制收敛），处置 fail-closed 中止');
    }
    if (!isChildOfProject(childLog.genesis, projectAffairId)) {
      throw new Error('目标事务的创世 refs 不含对本项目议题的 parent 引用——拒绝把处置写入不相干事务（append-only 不可收回）');
    }
    const childType = typeof childLog.genesis.type === 'string' ? childLog.genesis.type : '';
    const motion = await this.submitContentOp(childAffairId, buildDispositionPayload(action, note));
    if (childType === 'pr' || motion.status !== 'accepted') {
      // PR = 回执即生效（档一-3）；动议未被接受时不续投决议（countedOps 复算必败）
      return { opHash: motion.opHash, status: motion.status, resolutionOpHash: null };
    }
    const childRules = await affairs.readRules(childAffairId);
    const pubPeriodRaw = (childRules.current.rules as Record<string, unknown>).pubPeriod as
      | Record<string, unknown>
      | undefined;
    const pubPeriodMs = typeof pubPeriodRaw?.delayMs === 'number' ? pubPeriodRaw.delayMs : PROJECT_PUB_PERIOD_MS;
    const draft = buildResolutionDraft(
      childAffairId,
      actor,
      {
        result: action,
        condition: buildDispositionCloseCondition(),
        countedOps: [motion.opHash],
        rulesHash: childRules.current.rulesHash,
        pubPeriodMs
      },
      motion.opHash,
      Date.now()
    );
    const resolution = await affairs.submitOp(await this.signRecord(draft));
    return { opHash: motion.opHash, status: resolution.status, resolutionOpHash: resolution.opHash };
  }

  // ------------------------------------------------------------------
  // 成员页（§4：阶梯名册内核推导原样呈现 + 维护者集合 + 公开履历入口）
  // ------------------------------------------------------------------

  async listMembers(projectAffairId: string): Promise<{ members: ProjectMemberView[]; voters: string[] }> {
    const affairs = this.requireAffairs();
    const [status, project] = await Promise.all([affairs.ladderStatus(projectAffairId), this.getProject(projectAffairId)]);
    const maintainers = new Set(project?.maintainers ?? []);
    return {
      members: status.entries.map((entry) => ({
        identity: entry.identity,
        tier: entry.tier,
        accepts: entry.accepts,
        accountAgeMs: entry.accountAgeMs,
        lastActivityMs: entry.lastActivityMs,
        isMaintainer: maintainers.has(entry.identity)
      })),
      voters: status.voters
    };
  }

  /** 公开履历（sdk.affairs.publicProfile；本地副本所见如实标注由视图层负责） */
  getPublicProfile(identity: string): Promise<AffairPublicProfile> {
    return this.requireAffairs().publicProfile(identity);
  }

  // ------------------------------------------------------------------
  // 项目文档（§3.2：append-only 版本链；档三-3 不进事务决议）
  // ------------------------------------------------------------------

  async listDocVersions(projectAffairId: string): Promise<ProjectDocVersion[]> {
    await this.ensureCollections();
    const response = await this.sdk.data.query<ProjectDocVersion>(DOCS_COLLECTION, {
      prefix: `${projectAffairId}/`,
      limit: 2000
    });
    return response.items
      .map((item) => item.value)
      .filter((version) => version && version.projectAffairId === projectAffairId);
  }

  async listDocs(projectAffairId: string): Promise<ProjectDocSummary[]> {
    return deriveDocSummaries(await this.listDocVersions(projectAffairId));
  }

  async getDocHistory(projectAffairId: string, docId: string): Promise<ProjectDocVersion[]> {
    return docHistory(await this.listDocVersions(projectAffairId), docId);
  }

  /** 保存文档新版本（docId 缺省 = 新文档；seq = 既有最大 seq + 1） */
  async saveDoc(
    projectAffairId: string,
    input: { docId?: string; title: string; body: string }
  ): Promise<ProjectDocVersion> {
    const verdict = validateDocInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    const actor = await this.ensureActor();
    const docId = input.docId ?? newId('doc');
    await this.ensureCollections();
    // S3：并发写同 seq 时 append-only 集合拒绝覆盖——重读历史重算 seq 重试一次
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const history = await this.getDocHistory(projectAffairId, docId);
      const version: ProjectDocVersion = {
        projectAffairId,
        docId,
        seq: history.length > 0 ? history[history.length - 1].seq + 1 : 1,
        title: normalizeProjectText(input.title),
        body: input.body,
        authorIdentity: actor.identity,
        createdAt: Date.now()
      };
      try {
        await this.sdk.data.save(DOCS_COLLECTION, `${projectAffairId}/${docId}/${version.seq}`, version);
        return version;
      } catch (error) {
        const conflict = /append-?only|冲突|conflict/i.test((error as Error).message ?? '');
        if (!conflict || attempt === 1) {
          throw error;
        }
      }
    }
    throw new Error('unreachable');
  }

  // ------------------------------------------------------------------
  // 本地草稿（scope=local；未提交的子事务草稿，§3.2）
  // ------------------------------------------------------------------

  async saveDraft(draft: {
    id?: string;
    projectAffairId: string;
    type: ChildAffairType;
    title: string;
    summary: string;
    bundleCid?: string;
  }): Promise<string> {
    await this.ensureCollections();
    const id = draft.id ?? newId('draft');
    await this.sdk.data.save(DRAFTS_COLLECTION, id, { ...draft, id, savedAt: Date.now() });
    return id;
  }

  async listDrafts(projectAffairId: string): Promise<Array<Record<string, unknown>>> {
    await this.ensureCollections();
    const response = await this.sdk.data.query<Record<string, unknown>>(DRAFTS_COLLECTION, { limit: 200 });
    return response.items
      .map((item) => item.value)
      .filter((draft) => draft && draft.projectAffairId === projectAffairId);
  }

  async deleteDraft(draftId: string): Promise<void> {
    await this.ensureCollections();
    await this.sdk.data.delete(DRAFTS_COLLECTION, draftId);
  }

  // ------------------------------------------------------------------
  // 看板组合（库依赖：vendor spark-kanban；namespace='spark-project'）
  // MVP = 只读形态（§7.8：按子事务状态聚合分列，不做拖动转列）
  // ------------------------------------------------------------------

  private kanbanService(): KanbanService {
    this.kanban ??= new KanbanService(this.sdk, 'spark-project');
    return this.kanban;
  }

  private async workspaceOf(projectAffairId: string): Promise<{ boardId?: string }> {
    await this.ensureCollections();
    return (await this.sdk.data.get<{ boardId?: string }>(WORKSPACE_COLLECTION, projectAffairId)) ?? {};
  }

  /**
   * 项目看板视图（只读）：看板配置写本插件命名空间（§4「看板配置写本
   * 插件命名空间」）；卡片 = 项目子事务聚合（KanbanService.listAffairCards
   * 自动绑定主路径，反馈回流零成本落进待分诊列）。
   * 返回 null = 看板尚未初始化（视图层给初始化入口）。
   */
  async projectBoardView(projectAffairId: string, spaceId: string): Promise<{
    board: KanbanBoard;
    columns: KanbanColumnView[];
  } | null> {
    const kanban = this.kanbanService();
    const workspace = await this.workspaceOf(projectAffairId);
    const boards = await kanban.loadBoards(spaceId);
    const board =
      (workspace.boardId ? boards.find((item) => item.id === workspace.boardId) : undefined) ??
      boards.find((item) => item.contextAffairId === projectAffairId) ??
      null;
    if (!board) {
      return null;
    }
    const cards = await kanban.listAffairCards(board, []);
    return { board, columns: buildBoardView(board, [], cards, null) };
  }

  /**
   * 初始化项目看板（列模板 = 库件默认：待分诊/待办/进行中/待验证/完成）。
   * 组织空间建板受库件自身治理约束（仅组织管理员；个人空间放开）——
   * 被拒时如实上报原因，不静默、不绕过。
   */
  async initProjectBoard(
    projectAffairId: string,
    spaceId: string,
    rootId: string,
    role: 'admin' | 'member' | null,
    projectTitle: string
  ): Promise<BoardInitResult> {
    try {
      const board = await this.kanbanService().createBoard(
        spaceId,
        rootId,
        { name: `${projectTitle} · 看板`, contextAffairId: projectAffairId },
        role
      );
      await this.ensureCollections();
      await this.sdk.data.save(WORKSPACE_COLLECTION, projectAffairId, {
        boardId: board.id,
        updatedAt: Date.now()
      });
      return { ok: true, board };
    } catch (error) {
      return { ok: false, reason: (error as Error).message };
    }
  }

  // ------------------------------------------------------------------
  // 发布组合（库依赖：vendor spark-release-manager；namespace='spark-project'）
  // 发布管理件仅组织空间（其 manifest supportedSpaces=["org"]）；个人空间
  // 视图层显示「未启用」。版本卡片推送归发布管理件（档一-2），本插件不推。
  // ------------------------------------------------------------------

  private releaseService(): ReleaseManagerService {
    this.releases ??= new ReleaseManagerService(this.sdk, 'spark-project');
    return this.releases;
  }

  get releaseMarketAvailable(): boolean {
    return this.releaseService().marketAvailable;
  }

  async loadReleaseConfig(orgId: string): Promise<ReleaseManagerConfig | null> {
    return this.releaseService().loadConfig(orgId);
  }

  /** 初始化发布权配置（档三-23：MVP 名册管理员直改） */
  async initReleaseConfig(orgId: string, rootId: string, role: 'admin' | 'member' | null): Promise<ReleaseManagerConfig> {
    return this.releaseService().saveConfig(orgId, rootId, { publisherRootIds: [rootId] }, role);
  }

  /** 发布单列表（读侧鉴权：状态派生只认发布权集合 ∪ 名册管理员的事件） */
  async listReleaseRows(
    orgId: string,
    adminRootIds: string[]
  ): Promise<{ rows: ReleaseRow[]; config: ReleaseManagerConfig | null }> {
    const service = this.releaseService();
    const [config, releases, events] = await Promise.all([
      service.loadConfig(orgId),
      service.loadReleases(orgId),
      service.loadEvents(orgId)
    ]);
    const operatorIds = releaseEventOperatorSet(config, adminRootIds);
    const authorized = filterAuthorizedReleaseEvents(events, operatorIds);
    const rows = releases.map((release) => {
      const state = deriveReleaseState(release.id, authorized);
      return { release, state, stateLabel: RELEASE_STATE_LABELS[state] };
    });
    return { rows, config };
  }

  /** 登记发布单（发布权集合成员；包哈希随 governance 集合条目入存证链） */
  async registerRelease(
    orgId: string,
    rootId: string,
    input: {
      pluginId: string;
      version: string;
      updateManifestJson?: string;
      changelog?: string;
      decisionRef?: string;
    }
  ): Promise<ReleaseRecord> {
    const config = await this.loadReleaseConfig(orgId);
    return this.releaseService().registerRelease(orgId, rootId, input, config);
  }

  /** 推进「已发布」（业务层硬约束：核验未过不得推进；发布后即时推卡片） */
  async publishRelease(orgId: string, rootId: string, releaseId: string): Promise<ReleaseEvent> {
    const config = await this.loadReleaseConfig(orgId);
    return this.releaseService().publishRelease(orgId, rootId, releaseId, config);
  }

  /**
   * 核验编排（档一-6 本机导入复算；库件状态机硬约束：published 仅允许自
   * verified 推进——组合形态必须暴露本入口，否则「推进发布」永远不可达）。
   * 无市场模块（移动端/未授权）时库件报错说明委托桌面端成员核验（档二-8）。
   */
  async verifyRelease(
    orgId: string,
    rootId: string,
    releaseId: string,
    spkgPath: string
  ): Promise<ReleaseEvent> {
    const config = await this.loadReleaseConfig(orgId);
    return this.releaseService().verifyRelease(orgId, rootId, releaseId, { spkgPath }, config);
  }

  /** 发布单详情（release-card 视图按 cardData 引用 {releaseId, orgId} 读取） */
  async getReleaseDetail(
    orgId: string,
    releaseId: string,
    adminRootIds: string[]
  ): Promise<{ release: ReleaseRecord; state: ReleaseState; stateLabel: string } | null> {
    const service = this.releaseService();
    const [config, release, events] = await Promise.all([
      service.loadConfig(orgId),
      service.getRelease(orgId, releaseId),
      service.loadEvents(orgId)
    ]);
    if (!release) {
      return null;
    }
    const authorized = filterAuthorizedReleaseEvents(events, releaseEventOperatorSet(config, adminRootIds));
    const state = deriveReleaseState(release.id, authorized);
    return { release, state, stateLabel: RELEASE_STATE_LABELS[state] };
  }

  /** 存证链状态（发布详情「包哈希存证锚可点查」的载体；免权限只读） */
  getReleaseEvidence(): Promise<{ headHash: string | null; chainValid: boolean | null; chainHeight: number | null }> {
    return this.releaseService().getEvidenceStatus();
  }

  /**
   * 成员侧版本卡片补发（档二-4 MVP 降级 = 插件加载时补发 + 节流；
   * releaseRef 幂等键台账去重；配置不可得 fail-closed 不补发）。
   */
  async backfillReleaseCards(orgId: string, adminRootIds: string[]): Promise<number> {
    const service = this.releaseService();
    const config = await service.loadConfig(orgId);
    const operatorIds = config
      ? releaseEventOperatorSet(config, adminRootIds)
      : null;
    const [releases, events] = await Promise.all([service.loadReleases(orgId), service.loadEvents(orgId)]);
    return service.notifyNewReleases(orgId, releases, events, operatorIds);
  }

  // ------------------------------------------------------------------
  // 通知（message:app 增强能力；档二-4 降级：加载时补发 + 节流 + 台账去重）
  // ------------------------------------------------------------------

  /** 项目动态卡片（本机应用会话；权限被拒/限流降级返回 false，不阻断主流程） */
  private async notifyProjectCard(data: {
    kind: 'project-created' | 'child-disposition';
    affairId: string;
    title: string;
    action?: DispositionAction;
  }): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    const summary =
      data.kind === 'project-created'
        ? `[项目已创建]「${data.title}」议题已建立${data.affairId ? `（${data.affairId.slice(0, 12)}…）` : ''}。`
        : data.action
          ? `[子事务${DISPOSITION_LABELS[data.action]}]「${data.title}」已被维护者${DISPOSITION_LABELS[data.action].slice(1)}。`
          : `[子事务处置动态]「${data.title}」有新的处置操作——详情在「项目」插件中查看。`;
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: summary.slice(0, 200).trimEnd(), ...data },
        { viewId: 'affair-card', data }
      );
      return true;
    } catch (error) {
      console.warn('[spark-project] 动态卡片发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /**
   * 加载时补发（background 视图的 MVP 降级，档二-4）：扫描关注项目的子事务
   * 处置操作，对本地台账未记录者生成动态卡片。节流：单次加载至多 3 张，
   * 其余记账留待下次；限流/权限拒绝即中止本轮。
   */
  async backfillDispositionCards(): Promise<number> {
    if (!this.affairs || !this.sdk.messages) {
      return 0;
    }
    await this.ensureCollections();
    const ledger = await this.sdk.data.query<{ at?: number }>(NOTIFIED_COLLECTION, { prefix: 'disp:', limit: 2000 });
    const notified = new Set(ledger.items.map((item) => item.key));
    const projects = await this.listProjects();
    let sent = 0;
    for (const project of projects) {
      if (sent >= 3) {
        break;
      }
      // 处置推导（含 parent 规则写权过滤）已在 listChildren 内完成
      const children = await this.listChildren(project.affairId);
      for (const child of children) {
        if (sent >= 3) {
          break;
        }
        const disposition = child.disposition;
        if (disposition.state === 'open' || !disposition.opHash) {
          continue;
        }
        const key = `disp:${disposition.opHash}`;
        if (notified.has(key)) {
          continue;
        }
        const ok = await this.notifyProjectCard({
          kind: 'child-disposition',
          affairId: child.affairId,
          title: child.title,
          action: disposition.state
        });
        // 限流/权限降级：本轮中止，未记账的下次补齐
        if (!ok) {
          return sent;
        }
        await this.sdk.data.save(NOTIFIED_COLLECTION, key, { at: Date.now() });
        notified.add(key);
        sent += 1;
      }
    }
    return sent;
  }
}
