/**
 * 发布管理插件（spark-release-manager）· 业务服务层（release-management.md v0.2 + 拍板口径）。
 *
 * 架构纪律（沿用 spark-announcement/spark-kanban「服务层集中 SDK 调用」）：所有
 * SDK 调用集中在服务层，视图组件不直接碰 sdk.docs——便于单测（tests/ 用 mock
 * SDK 驱动本层）与权限审计（本文件即插件能力面清单）。
 *
 * 本层协作的 SDK 能力：
 *   1) docs（storage:read/write）：集合声明 + 文档读写——发布单 releases 与状态
 *      事件 release_events 声明 governance 语义（强制 append-only + 链式存证，
 *      dev-guide §6），包哈希随发布单条目天然入存证链，无需插件自建哈希链；
 *   2) identity（identity:sign）：发布单/状态事件防抵赖签名，验签免权限；
 *   3) market（market:read）：本机导入复算——inspectLocal 由内核复算 .spkg 整包
 *      sha256/size 并解析容器内 manifest（plugin-dist §5 同口径），本插件只验
 *      不签、不持有私钥；checkUpdates 供「当前版本 → 最新版本」对照视图；
 *   4) messages（message:app）：版本卡片唯一推送源（档一-2）——发布者路径只是
 *      发布者本机即时反馈；成员侧按服务号模型（p2p-messages §20.4.3）在同步后
 *      从本机数据「本地生成」卡片，同步的是数据不是消息；
 *   5) evidence（免权限只读）：详情页存证状态（链头哈希 / 校验），不伪造时效；
 *   6) data（storage）：送达台账 scope=local（iframe opaque origin 恒无
 *      localStorage，台账一律走 sdk.data，插件群统一口径）。
 *
 * 边界红线：构建在 CI，本插件只登记/编排/跟踪；签名验签是内核/协议能力——
 * 验签信任链与内核市场一致（内核市场通路执行），本插件不重实现验签。
 *
 * 拍板口径落点：
 * - 档一-2：MVP 版本卡片由本插件唯一推送，releaseRef（= 发布单记录 id，档三-24）
 *   为幂等键；契约落地后收敛到公告插件统一通道；
 * - 档一-6：核验 = 本机导入复算覆盖；在线抓取复算（network:fetch）非验收要件；
 * - 档二-8：在线抓取复算（network:fetch，桌面限定）排后续迭代——MVP 不声明
 *   network:fetch（未使用且授权层无平台门控）；移动端（或 market 模块缺席）
 *   降级只做登记，委托桌面端成员核验；
 * - 档三-23：发布权集合由名册管理员直改初始化（saveConfig）；
 * - 档三-25：版本上报 opt-in 专用集合 + 分布视图骨架（如实标注非全量）；
 * - 档三-26：撤回 = 组织内登记（append-only 撤回事件）+ 公告告知（撤回卡片），
 *   不回滚已安装、不删原记录。
 *
 * 库包形态（§5.1，同 spark-kanban 纪律）：被「项目」等插件构建期组合时，组合者
 * 以其自身插件 id 构造 ReleaseManagerService（namespace 参数）——sdk.docs 集合
 * 本插件命名空间由内核按调用方域隔离（数据自动落到组合者域）；sdk.data 送达
 * 台账名内核强制前缀 == 调用方插件 id，故由 namespace 参数构造。
 */
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildReleaseCardSummary,
  buildReleaseHistorySummary,
  buildReleaseSignPayload,
  buildRetractionSummary,
  canAppendEvent,
  canManageReleaseConfig,
  canPublishRelease,
  canRetractRelease,
  compareReleasePackage,
  deriveReleaseState,
  filterAuthorizedReleaseEvents,
  hasSignatureMaterial,
  normalizeReleaseText,
  parseUpdateManifest,
  releaseEventSignContent,
  releaseSignContent,
  selectReleaseBackfillBatch,
  validateArtifacts,
  validateChangelog,
  validatePluginRef,
  validateReason,
  validateVersion,
  RELEASE_CHANNEL_KINDS,
  RELEASE_CHANNEL_KIND_LABELS,
  RELEASE_MAX_CHANNEL_NOTE_LENGTH,
  RELEASE_MAX_CHANNEL_TARGET_LENGTH,
  type RecomputedPackage,
  type ReleaseArtifact,
  type ReleaseChannel,
  type ReleaseChannelKind,
  type ReleaseEvent,
  type ReleaseEventType,
  type ReleaseManagerConfig,
  type ReleaseRecord,
  type ReleaseSignature,
  type ReleaseState,
  type ReleaseVerificationDetail,
  type VersionReport
} from './model';

export type { ReleaseManagerConfig } from './model';

// ------------------------------------------------------------------
// 集合声明（sdk.docs；写入前必须声明，启动时统一声明一次，声明幂等）
// ------------------------------------------------------------------

export const RELEASE_COLLECTIONS = {
  config: 'release_config',
  releases: 'release_releases',
  events: 'release_events',
  channels: 'release_channels',
  reports: 'release_version_reports'
} as const;

/**
 * 集合同步策略声明：
 * - releases / events：governance 语义（governance: true → 强制 append-only +
 *   链式存证，插件无权降级）——「包哈希进存证链」由集合声明天然达成（§3 挂接点 1）；
 * - reports：append-only（版本上报只增不改；档三-25 骨架）；
 * - channels / config：lww（渠道与发布权配置可被后续调整覆盖，显式声明）。
 */
const RELEASE_COLLECTION_SCHEMAS = {
  [RELEASE_COLLECTIONS.releases]: { syncStrategy: 'append-only', governance: true },
  [RELEASE_COLLECTIONS.events]: { syncStrategy: 'append-only', governance: true },
  [RELEASE_COLLECTIONS.reports]: { syncStrategy: 'append-only' },
  [RELEASE_COLLECTIONS.channels]: { syncStrategy: 'lww' },
  [RELEASE_COLLECTIONS.config]: { syncStrategy: 'lww' }
} as const;

/**
 * 送达台账集合名（sdk.data scope:'local'；内核 plugindata 强制前缀 == 调用方
 * 插件 id，故由 namespace 构造——独立安装形态缺省，被组合时 = 组合者插件 id）。
 */
function deliveryCollectionName(namespace: string): string {
  return `${namespace}:delivery`;
}

type OrgRole = 'admin' | 'member' | null | undefined;

/**
 * 记录 id：时间戳 + 进程内单调计数 + 随机后缀（同 spark-kanban：同毫秒连续
 * 写入的 id 字典序与写入序一致，append-only 折叠的 at+id tie-break 稳定）。
 */
let idCounter = 0;

function newId(prefix: string): string {
  idCounter = (idCounter + 1) & 0xffffff;
  return `${prefix}_${Date.now()}_${idCounter.toString(16).padStart(6, '0')}_${Math.random().toString(16).slice(2, 10)}`;
}

/** 信任链诚实标注（档一-6/边界红线；随核验证据入链，不伪造「已验签」） */
const KERNEL_TRUST_NOTE =
  '包签名验签在内核市场通路执行（内置目录签名链路 / 仓库锚定安装，plugin-dist §4.3），' +
  '本插件只验不签、不持有私钥、不重实现验签；本事件登记的核验证据 = 本机导入复算' +
  '（内核复算整包哈希/size）与发布单登记值/update-manifest 的三方比对。';

/** 送达熔断观测面（与公告插件同口径：超限计入 rejectedCount，进程内累计） */
export type ReleaseDeliveryStats = {
  sentCount: number;
  rejectedCount: number;
};

export class ReleaseManagerService {
  private collectionsReady: Promise<void> | null = null;
  private deliveryCollectionReady: Promise<void> | null = null;
  private readonly stats: ReleaseDeliveryStats = { sentCount: 0, rejectedCount: 0 };
  /** 送达链路进程内 mutex（按空间串行化发布即时通知与加载时补发，防重复卡片） */
  private readonly deliveryChains = new Map<string, Promise<unknown>>();
  /** 送达台账集合名（sdk.data local；namespace = 调用方插件 id） */
  private readonly deliveryCollection: string;

  constructor(
    private readonly sdk: PluginSDK,
    namespace = 'spark-release-manager'
  ) {
    this.deliveryCollection = deliveryCollectionName(namespace);
  }

  /** 声明本插件全部集合的同步策略（幂等，重复声明与首次一致即可） */
  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(RELEASE_COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  /** 声明送达台账集合（sdk.data scope:'local'，幂等；代际内策略冲突报错） */
  private ensureDeliveryCollection(): Promise<void> {
    this.deliveryCollectionReady ??= this.sdk.data
      .declareCollection({ name: this.deliveryCollection, scope: 'local' })
      .then(() => undefined);
    return this.deliveryCollectionReady;
  }

  /** 同一空间的送达操作串行化（前序失败不阻塞后续任务） */
  private enqueueDelivery<T>(orgId: string, task: () => Promise<T>): Promise<T> {
    const prev = this.deliveryChains.get(orgId) ?? Promise.resolve();
    const next = prev.then(task, task);
    this.deliveryChains.set(orgId, next);
    return next;
  }

  /** 送达熔断观测面（rejectedCount 等，进程内累计） */
  getDeliveryStats(): ReleaseDeliveryStats {
    return { ...this.stats };
  }

  /** 市场模块可用性（档二-8：移动端/无 market 模块时降级只做登记，不报错） */
  get marketAvailable(): boolean {
    return Boolean(this.sdk.market);
  }

  // ------------------------------------------------------------------
  // 送达台账（releaseRef = 发布单记录 id 为幂等键，档一-2/档三-24）
  // ------------------------------------------------------------------

  /**
   * 读送达台账（sdk.data 持久面；持久面不可用时降级进程内集合——本会话内去重）。
   * 键：`{orgId}:{releaseId}`（发布卡片）与 `{orgId}:{releaseId}:retract`（撤回告知）。
   */
  private readonly memoryDeliveredFallback = new Map<string, Set<string>>();

  private async loadDeliveredIds(orgId: string): Promise<Set<string>> {
    try {
      await this.ensureDeliveryCollection();
      const response = await this.sdk.data.query<{ at?: number }>(this.deliveryCollection, {
        prefix: `${orgId}:`,
        limit: 2000
      });
      const ids = new Set(response.items.map((item) => item.key.slice(orgId.length + 1)));
      this.memoryDeliveredFallback.set(orgId, new Set(ids));
      return ids;
    } catch (error) {
      console.warn('[spark-release-manager] 送达台账持久面读取失败，降级会话内去重：', error);
      return new Set(this.memoryDeliveredFallback.get(orgId) ?? []);
    }
  }

  /** 记送达台账：持久面为主，进程内兜底同步刷新（写失败不阻塞送达流程） */
  private async markDelivered(orgId: string, key: string, at: number): Promise<void> {
    const cached = this.memoryDeliveredFallback.get(orgId) ?? new Set<string>();
    cached.add(key);
    this.memoryDeliveredFallback.set(orgId, cached);
    try {
      await this.ensureDeliveryCollection();
      await this.sdk.data.save(this.deliveryCollection, `${orgId}:${key}`, { at });
    } catch (error) {
      console.warn('[spark-release-manager] 送达台账持久面写入失败（已降级为会话内去重）：', error);
    }
  }

  // ------------------------------------------------------------------
  // 发布权配置（档三-23：MVP 名册管理员直改初始化）
  // ------------------------------------------------------------------

  async loadConfig(orgId: string): Promise<ReleaseManagerConfig | null> {
    await this.ensureCollectionsDeclared();
    return this.sdk.docs.get<ReleaseManagerConfig>(RELEASE_COLLECTIONS.config, orgId);
  }

  /**
   * 初始化/直改发布权配置（lww 单文档，文档 id = orgId）。仅名册管理员；
   * 发布权（供应链敏感）的后续变更挂组织治理事务，排「规则挂事务」迭代（档三-23）。
   */
  async saveConfig(
    orgId: string,
    rootId: string,
    input: { publisherRootIds: string[] },
    role: OrgRole
  ): Promise<ReleaseManagerConfig> {
    if (!canManageReleaseConfig(role)) {
      throw new Error('仅组织管理员可以维护发布权配置');
    }
    await this.ensureCollectionsDeclared();
    const existing = await this.loadConfig(orgId);
    const publisherRootIds = [...new Set(input.publisherRootIds.map((item) => item.trim()).filter(Boolean))];
    if (publisherRootIds.length === 0) {
      throw new Error('发布权集合不能为空（至少一名发布者；否则任何成员都无登记路径）');
    }
    const config: ReleaseManagerConfig = {
      orgId,
      publisherRootIds,
      createdBy: existing?.createdBy ?? rootId,
      createdAt: existing?.createdAt ?? Date.now(),
      updatedAt: Date.now()
    };
    await this.sdk.docs.put(RELEASE_COLLECTIONS.config, orgId, config as unknown as Record<string, unknown>);
    return config;
  }

  // ------------------------------------------------------------------
  // 签名（identity:sign 防抵赖；验签免权限）
  // ------------------------------------------------------------------

  /**
   * 域身份签名（同公告/看板口径）：identity:sign 是「使用时询问」高危权限，
   * 用户拒绝时桥会抛错——降级为不签名（记录无「已签名」徽标），不阻断主流程。
   */
  private async signRecord(
    orgId: string,
    recordId: string,
    rootId: string,
    content: string
  ): Promise<ReleaseSignature | null> {
    const payload = buildReleaseSignPayload(orgId, recordId, rootId, content);
    try {
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-release-manager] 签名被拒或不可用，记录将不带签名徽标：', error);
      return null;
    }
  }

  /** 验签（identity.verify 免权限）：重算载荷比对 + 密码学验签（同 forum 口径） */
  private async verifySignature(
    orgId: string,
    recordId: string,
    rootId: string,
    content: string,
    signature: ReleaseSignature | undefined
  ): Promise<boolean> {
    if (!signature) {
      return false;
    }
    const expected = buildReleaseSignPayload(orgId, recordId, rootId, content);
    if (signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, signature.signature, signature.publicKey);
    return result.valid;
  }

  /** 发布单签名独立验签（验收⑤口径：sdk.identity.verify 重算比对） */
  verifyReleaseSignature(release: ReleaseRecord): Promise<boolean> {
    return this.verifySignature(
      release.orgId,
      release.id,
      release.publisherRootId,
      releaseSignContent(release),
      release.signature
    );
  }

  /** 状态事件签名独立验签 */
  verifyEventSignature(event: ReleaseEvent): Promise<boolean> {
    return this.verifySignature(
      event.orgId,
      event.id,
      event.operatorRootId,
      releaseEventSignContent(event),
      event.signature
    );
  }

  // ------------------------------------------------------------------
  // 发布单登记（releases，governance append-only；包哈希随条目入存证链）
  // ------------------------------------------------------------------

  /**
   * 登记发布单（发布权集合成员，业务层校验；配置未初始化 fail-closed）。
   * 输入可为「导入 update-manifest.json 自动解析」（updateManifestJson 原文）
   * 或人工录入的资产清单；两者都在服务层入口 fail-closed 全量校验（不能只靠
   * 视图层把关）。登记幂等键 = （组织, 目标插件, 版本号）：同键重复登记拒绝。
   * 登记即签名（防抵赖）后写入 governance 集合——发布单内容（含包哈希）随之
   * 进入存证链。
   */
  async registerRelease(
    orgId: string,
    rootId: string,
    input: {
      pluginId: string;
      version: string;
      /** CI 产出的 update-manifest.json 原文（提供则解析覆盖 artifacts） */
      updateManifestJson?: string;
      artifacts?: ReleaseArtifact[];
      changelog?: string;
      changelogRef?: string;
      decisionRef?: string;
      channels?: string[];
    },
    config: ReleaseManagerConfig | null
  ): Promise<ReleaseRecord> {
    if (!canPublishRelease(config, rootId)) {
      throw new Error('仅发布权集合成员可以登记发布单（请联系组织管理员初始化/维护发布权配置）');
    }
    const pluginCheck = validatePluginRef(input.pluginId);
    if (!pluginCheck.ok) {
      throw new Error(pluginCheck.reason ?? '目标插件引用不合法');
    }
    const versionCheck = validateVersion(input.version);
    if (!versionCheck.ok) {
      throw new Error(versionCheck.reason ?? '版本号不合法');
    }
    const changelogCheck = validateChangelog(input.changelog);
    if (!changelogCheck.ok) {
      throw new Error(changelogCheck.reason ?? '变更说明不合法');
    }

    // 登记向导主路径：导入 update-manifest.json 解析资产清单（人工核对后提交）
    let artifacts = input.artifacts ?? [];
    let updateManifest: ReleaseRecord['updateManifest'];
    if (input.updateManifestJson !== undefined) {
      const parsed = parseUpdateManifest(input.updateManifestJson);
      artifacts = parsed.artifacts;
      updateManifest = parsed.manifest;
    }
    const artifactCheck = validateArtifacts(artifacts);
    if (!artifactCheck.ok) {
      throw new Error(artifactCheck.reason ?? '资产清单不合法');
    }

    await this.ensureCollectionsDeclared();
    const pluginId = normalizeReleaseText(input.pluginId);
    const version = normalizeReleaseText(input.version);

    // 登记幂等键（档一-2：releaseRef/版本号幂等）：（组织, 插件, 版本）唯一
    const existing = await this.loadReleases(orgId);
    if (existing.some((release) => release.pluginId === pluginId && release.version === version)) {
      throw new Error(`发布单已存在：${pluginId} v${version}（releaseRef/版本号为幂等键，重复登记被拒绝）`);
    }

    const release: ReleaseRecord = {
      id: newId('rel'),
      orgId,
      pluginId,
      version,
      artifacts,
      ...(updateManifest ? { updateManifest } : {}),
      ...(input.changelog?.trim() ? { changelog: normalizeReleaseText(input.changelog) } : {}),
      ...(input.changelogRef?.trim() ? { changelogRef: input.changelogRef.trim() } : {}),
      ...(input.decisionRef?.trim() ? { decisionRef: input.decisionRef.trim() } : {}),
      channels: [...new Set((input.channels ?? []).map((item) => item.trim()).filter(Boolean))],
      publisherRootId: rootId,
      createdAt: Date.now()
    };

    const signature = await this.signRecord(orgId, release.id, rootId, releaseSignContent(release));
    if (signature) {
      release.signature = signature;
    }

    await this.sdk.docs.put(RELEASE_COLLECTIONS.releases, release.id, release as unknown as Record<string, unknown>);
    return release;
  }

  async loadReleases(orgId: string): Promise<ReleaseRecord[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<ReleaseRecord>(RELEASE_COLLECTIONS.releases, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 1000
    });
    return response.items
      .map((item) => item.data)
      .sort((a, b) => b.createdAt - a.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  async getRelease(orgId: string, releaseId: string): Promise<ReleaseRecord | null> {
    await this.ensureCollectionsDeclared();
    const release = await this.sdk.docs.get<ReleaseRecord>(RELEASE_COLLECTIONS.releases, releaseId);
    return release && release.orgId === orgId ? release : null;
  }

  // ------------------------------------------------------------------
  // 状态事件（release_events，governance append-only；状态机由事件流派生）
  // ------------------------------------------------------------------

  async loadEvents(orgId: string): Promise<ReleaseEvent[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<ReleaseEvent>(RELEASE_COLLECTIONS.events, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 2000
    });
    return response.items
      .map((item) => item.data)
      .sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  /** 追加状态事件（内部）：写侧守卫 + 操作者签名 + append-only 写入 */
  private async appendEvent(
    orgId: string,
    rootId: string,
    release: ReleaseRecord,
    draft: {
      type: ReleaseEventType;
      channelId?: string;
      resultRef?: string;
      reason?: string;
      detail?: string;
      verification?: ReleaseVerificationDetail;
    },
    events: ReleaseEvent[]
  ): Promise<ReleaseEvent> {
    const state = deriveReleaseState(release.id, events);
    const guard = canAppendEvent(state, draft.type);
    if (!guard.ok) {
      throw new Error(guard.reason ?? '状态推进被拒绝');
    }
    const event: ReleaseEvent = {
      id: newId('evt'),
      orgId,
      releaseId: release.id,
      type: draft.type,
      ...(draft.channelId ? { channelId: draft.channelId } : {}),
      ...(draft.resultRef?.trim() ? { resultRef: draft.resultRef.trim() } : {}),
      ...(draft.reason?.trim() ? { reason: normalizeReleaseText(draft.reason) } : {}),
      ...(draft.detail?.trim() ? { detail: normalizeReleaseText(draft.detail) } : {}),
      ...(draft.verification ? { verification: draft.verification } : {}),
      operatorRootId: rootId,
      at: Date.now()
    };
    const signature = await this.signRecord(orgId, event.id, rootId, releaseEventSignContent(event));
    if (signature) {
      event.signature = signature;
    }
    await this.sdk.docs.put(RELEASE_COLLECTIONS.events, event.id, event as unknown as Record<string, unknown>);
    return event;
  }

  /**
   * 核验编排（档一-6：本机导入复算；发布权集合成员操作）。
   * 流程：读取发布单 → sdk.market.inspectLocal 由内核复算 .spkg 整包
   * sha256/size 并解析容器内 manifest（plugin-dist §5 同口径）→ 与发布单登记
   * 资产 / update-manifest 三方比对（compareReleasePackage）→ 全部一致追加
   * 'verified' 事件（核验证据入链），任一不一致追加 'verify-failed' 事件
   * （失败原因原样记录，不替发布者遮掩）。
   *
   * 验签边界（诚实标注，随证据入链）：包签名验签在内核市场通路执行（信任链
   * 与内核市场一致），本插件只验不签、不重实现——MVP 核验证据 = 哈希复算
   * 三方比对 + 签名材料声明核对。
   *
   * 降级（档二-8）：market 模块缺席（移动端/未授权）→ 只做登记不核验，本方法
   * 直接报错说明「请委托桌面端成员核验」，不伪造核验结果。
   */
  async verifyRelease(
    orgId: string,
    rootId: string,
    releaseId: string,
    input: { spkgPath: string },
    config: ReleaseManagerConfig | null
  ): Promise<ReleaseEvent> {
    if (!canPublishRelease(config, rootId)) {
      throw new Error('仅发布权集合成员可以执行核验编排');
    }
    if (!this.sdk.market) {
      throw new Error('当前环境无市场模块（移动端或桌面限定能力未授权）——本机导入复算不可用，请只做登记并委托桌面端成员核验（档二-8 降级口径）');
    }
    const spkgPath = normalizeReleaseText(input.spkgPath);
    if (!spkgPath) {
      throw new Error('请提供 .spkg 包文件路径（本机导入复算）');
    }
    const release = await this.getRelease(orgId, releaseId);
    if (!release) {
      throw new Error('目标发布单不存在或尚未同步到本机');
    }
    const events = await this.loadEvents(orgId);

    // 内核复算（inspectLocal：容器解析 + 整包 sha256/size；失败原因原样入失败事件）
    let recomputed: RecomputedPackage | null = null;
    let inspectError: string | null = null;
    try {
      const preview = await this.sdk.market.inspectLocal(spkgPath);
      recomputed = {
        sha256: preview.sha256,
        size: preview.size,
        pluginId: preview.pluginId,
        version: preview.version
      };
    } catch (error) {
      inspectError = `.spkg 导入解析/复算失败：${(error as Error).message ?? String(error)}`;
    }

    const mismatches = recomputed ? compareReleasePackage(release, recomputed) : [inspectError ?? '导入复算失败'];
    const verification: ReleaseVerificationDetail = {
      fileName: spkgPath.split(/[\\/]/).pop() ?? spkgPath,
      ...(recomputed ? { recomputed } : {}),
      manifestAssetsChecked: release.updateManifest?.assets?.length ?? 0,
      mismatches,
      sigMaterialDeclared: hasSignatureMaterial(release.artifacts),
      trustNote: KERNEL_TRUST_NOTE
    };

    if (mismatches.length === 0 && recomputed) {
      return this.appendEvent(orgId, rootId, release, { type: 'verified', verification }, events);
    }
    return this.appendEvent(
      orgId,
      rootId,
      release,
      { type: 'verify-failed', reason: mismatches.join('；'), verification },
      events
    );
  }

  /**
   * 推进「已发布」（发布权集合成员；业务层硬约束：核验未过不得推进）。
   * 追加 'published' 事件后即时推版本卡片（发布者本机即时反馈；成员侧经同步
   * 后由 notifyNewReleases 本地生成）。卡片推送失败不阻断状态推进。
   */
  async publishRelease(
    orgId: string,
    rootId: string,
    releaseId: string,
    config: ReleaseManagerConfig | null,
    input: { detail?: string } = {}
  ): Promise<ReleaseEvent> {
    if (!canPublishRelease(config, rootId)) {
      throw new Error('仅发布权集合成员可以推进发布');
    }
    const release = await this.getRelease(orgId, releaseId);
    if (!release) {
      throw new Error('目标发布单不存在或尚未同步到本机');
    }
    const events = await this.loadEvents(orgId);
    const event = await this.appendEvent(orgId, rootId, release, { type: 'published', detail: input.detail }, events);
    // 版本卡片唯一推送源（档一-2）：发布者本机即时反馈；失败降级不阻断
    try {
      await this.notifyReleasePublished(release);
    } catch (error) {
      console.warn('[spark-release-manager] 版本卡片推送失败（不影响发布状态推进）：', error);
    }
    return event;
  }

  /**
   * 登记渠道推送（发布权集合成员；仅「已发布」发布单）。每条渠道动作 = 一条
   * append-only 记录（渠道、时间、操作者签名、结果引用）——渠道可追责（验收④）。
   * 本插件登记与跟踪渠道，分发执行是内核/协议的事。
   */
  async recordChannelPush(
    orgId: string,
    rootId: string,
    releaseId: string,
    input: { channelId: string; resultRef?: string; detail?: string },
    config: ReleaseManagerConfig | null
  ): Promise<ReleaseEvent> {
    if (!canPublishRelease(config, rootId)) {
      throw new Error('仅发布权集合成员可以登记渠道推送');
    }
    const channelId = normalizeReleaseText(input.channelId);
    if (!channelId) {
      throw new Error('请选择渠道');
    }
    const [release, channels, events] = await Promise.all([
      this.getRelease(orgId, releaseId),
      this.loadChannels(orgId),
      this.loadEvents(orgId)
    ]);
    if (!release) {
      throw new Error('目标发布单不存在或尚未同步到本机');
    }
    if (!channels.some((channel) => channel.id === channelId)) {
      throw new Error(`渠道 ${channelId} 未登记（请先在渠道区登记渠道）`);
    }
    return this.appendEvent(
      orgId,
      rootId,
      release,
      { type: 'channel-pushed', channelId, resultRef: input.resultRef, detail: input.detail },
      events
    );
  }

  /**
   * 撤回发布单（档三-26：发布权集合成员或名册管理员；撤回 = 组织内登记 +
   * 公告告知，不回滚已安装、不删原记录——已扩散的包不可收回）。追加 'retracted'
   * 事件（append-only）并推撤回告知卡片；内核市场侧「撤回清单」机制未见权威
   * 口径，已提协议守护 backlog。
   */
  async retractRelease(
    orgId: string,
    rootId: string,
    releaseId: string,
    input: { reason?: string },
    config: ReleaseManagerConfig | null,
    role: OrgRole
  ): Promise<ReleaseEvent> {
    if (!canRetractRelease(config, rootId, role)) {
      throw new Error('仅发布权集合成员或组织管理员可以撤回发布单');
    }
    const reasonCheck = validateReason(input.reason, '撤回理由');
    if (!reasonCheck.ok) {
      throw new Error(reasonCheck.reason ?? '撤回理由不合法');
    }
    const release = await this.getRelease(orgId, releaseId);
    if (!release) {
      throw new Error('目标发布单不存在或尚未同步到本机');
    }
    const events = await this.loadEvents(orgId);
    const event = await this.appendEvent(orgId, rootId, release, { type: 'retracted', reason: input.reason }, events);
    // 公告告知（档三-26）：撤回告知卡片，发布者本机即时反馈；失败降级不阻断
    try {
      await this.notifyReleaseRetracted(release, event);
    } catch (error) {
      console.warn('[spark-release-manager] 撤回告知卡片推送失败（不影响撤回登记）：', error);
    }
    return event;
  }

  // ------------------------------------------------------------------
  // 渠道（channels，lww-record；声明性登记，不携带执行凭据）
  // ------------------------------------------------------------------

  async loadChannels(orgId: string): Promise<ReleaseChannel[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<ReleaseChannel>(RELEASE_COLLECTIONS.channels, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 500
    });
    return response.items
      .map((item) => item.data)
      .sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  /** 登记/更新渠道（发布权集合成员或名册管理员；lww 覆盖语义符合「当前生效配置」直觉） */
  async saveChannel(
    orgId: string,
    rootId: string,
    input: { id?: string; kind: ReleaseChannelKind; target: string; note?: string },
    config: ReleaseManagerConfig | null,
    role: OrgRole
  ): Promise<ReleaseChannel> {
    if (!canPublishRelease(config, rootId) && role !== 'admin') {
      throw new Error('仅发布权集合成员或组织管理员可以登记渠道');
    }
    if (!RELEASE_CHANNEL_KINDS.includes(input.kind)) {
      throw new Error(`渠道类型非法（可选：${RELEASE_CHANNEL_KINDS.map((kind) => RELEASE_CHANNEL_KIND_LABELS[kind]).join(' / ')}）`);
    }
    const target = normalizeReleaseText(input.target);
    if (!target) {
      throw new Error('渠道目标不能为空（目录条目 / 声明文件地址 / announce topic / 归档位置）');
    }
    if (target.length > RELEASE_MAX_CHANNEL_TARGET_LENGTH) {
      throw new Error(`渠道目标长度不能超过${RELEASE_MAX_CHANNEL_TARGET_LENGTH}字`);
    }
    if (input.note && input.note.length > RELEASE_MAX_CHANNEL_NOTE_LENGTH) {
      throw new Error(`渠道备注长度不能超过${RELEASE_MAX_CHANNEL_NOTE_LENGTH}字`);
    }
    await this.ensureCollectionsDeclared();
    const now = Date.now();
    const id = input.id?.trim() || newId('chan');
    const existing = input.id?.trim()
      ? ((await this.sdk.docs.get<ReleaseChannel>(RELEASE_COLLECTIONS.channels, id)) ?? null)
      : null;
    const channel: ReleaseChannel = {
      id,
      orgId,
      kind: input.kind,
      target,
      ...(input.note?.trim() ? { note: normalizeReleaseText(input.note) } : {}),
      createdBy: existing?.createdBy ?? rootId,
      createdAt: existing?.createdAt ?? now,
      updatedAt: now
    };
    await this.sdk.docs.put(RELEASE_COLLECTIONS.channels, channel.id, channel as unknown as Record<string, unknown>);
    return channel;
  }

  // ------------------------------------------------------------------
  // 版本上报（version_reports，append-only；档三-25 MVP 骨架：opt-in 上报 +
  // 分布视图如实标注非全量；不含设备指纹）
  // ------------------------------------------------------------------

  /** opt-in 上报本机运行版本（任何组织成员；最小字段） */
  async reportVersion(
    orgId: string,
    rootId: string,
    input: { pluginId: string; version: string; trust: string },
    role: OrgRole
  ): Promise<VersionReport> {
    if (role !== 'admin' && role !== 'member') {
      throw new Error('仅组织成员可以上报本机版本');
    }
    const pluginCheck = validatePluginRef(input.pluginId);
    if (!pluginCheck.ok) {
      throw new Error(pluginCheck.reason ?? '目标插件引用不合法');
    }
    const version = normalizeReleaseText(input.version);
    if (!version || version.length > 40) {
      throw new Error('运行版本号不能为空且长度不能超过 40 字');
    }
    const trust = normalizeReleaseText(input.trust);
    if (!trust) {
      throw new Error('信任级不能为空（signed / repo-anchored / sideloaded / builtin，原样上报不美化）');
    }
    await this.ensureCollectionsDeclared();
    const report: VersionReport = {
      id: newId('vrpt'),
      orgId,
      pluginId: normalizeReleaseText(input.pluginId),
      version,
      trust,
      reporterRootId: rootId,
      at: Date.now()
    };
    await this.sdk.docs.put(RELEASE_COLLECTIONS.reports, report.id, report as unknown as Record<string, unknown>);
    return report;
  }

  async loadVersionReports(orgId: string): Promise<VersionReport[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<VersionReport>(RELEASE_COLLECTIONS.reports, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 2000
    });
    return response.items
      .map((item) => item.data)
      .sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  }

  // ------------------------------------------------------------------
  // 存证状态（sdk.evidence 免权限只读；详情页展示，不伪造时效）
  // ------------------------------------------------------------------

  /**
   * 当前存证链状态（§3 挂接点 2/3）：链头哈希 + 链校验结果。锚定状态如实
   * 分态——headHash 为 null 表示本机尚无任何入链记录；锚定时刻以内核锚定
   * 机制为准（治理事件驱动 + 每日兜底），本面不伪造「已锚定」。
   */
  async getEvidenceStatus(): Promise<{ headHash: string | null; chainValid: boolean | null; chainHeight: number | null }> {
    const head = await this.sdk.evidence.headHash();
    let chainValid: boolean | null = null;
    let chainHeight: number | null = null;
    try {
      const result = await this.sdk.evidence.verify();
      chainValid = result.valid;
      chainHeight = result.height;
    } catch (error) {
      console.warn('[spark-release-manager] 存证链校验不可用（降级只展示链头哈希）：', error);
    }
    return { headHash: head.hash, chainValid, chainHeight };
  }

  // ------------------------------------------------------------------
  // 版本卡片推送（message:app；本插件唯一推送源，档一-2；本地生成本地消费）
  // ------------------------------------------------------------------

  /**
   * 为一条已发布发布单生成本机应用会话卡片（summary 强制 + release-card 卡片，
   * data 只携带引用 {releaseId, orgId}——发布单经 docs 查询，不随消息冗余落库）。
   * 发布者路径只是发布者本机的即时反馈；成员侧走 notifyNewReleases 本地生成。
   * 降级：权限被拒/内核限流（10 条/60s）时不阻断，计入 rejectedCount，返回 false。
   */
  async notifyReleasePublished(release: ReleaseRecord): Promise<boolean> {
    return this.enqueueDelivery(release.orgId, async () => {
      if (!this.sdk.messages) {
        return false;
      }
      try {
        await this.sdk.messages.sendAppMessage(
          {
            summary: buildReleaseCardSummary(release),
            releaseRef: release.id,
            pluginId: release.pluginId,
            version: release.version,
            orgId: release.orgId
          },
          { viewId: 'release-card', data: { releaseId: release.id, orgId: release.orgId } }
        );
        await this.markDelivered(release.orgId, release.id, Date.now());
        this.stats.sentCount += 1;
        return true;
      } catch (error) {
        this.stats.rejectedCount += 1;
        console.warn('[spark-release-manager] 版本卡片发送失败（权限/限流降级）：', error);
        return false;
      }
    });
  }

  /**
   * 为一条撤回发布单生成撤回告知卡片（档三-26：公告告知；幂等键
   * `{releaseId}:retract`——撤回不回滚已送达的发布卡片，另发告知）。
   */
  async notifyReleaseRetracted(release: ReleaseRecord, event: ReleaseEvent): Promise<boolean> {
    return this.enqueueDelivery(release.orgId, async () => {
      if (!this.sdk.messages) {
        return false;
      }
      try {
        await this.sdk.messages.sendAppMessage(
          {
            summary: buildRetractionSummary({ pluginId: release.pluginId, version: release.version, reason: event.reason }),
            releaseRef: release.id,
            retracted: true,
            pluginId: release.pluginId,
            version: release.version,
            orgId: release.orgId
          },
          { viewId: 'release-card', data: { releaseId: release.id, orgId: release.orgId } }
        );
        await this.markDelivered(release.orgId, `${release.id}:retract`, Date.now());
        this.stats.sentCount += 1;
        return true;
      } catch (error) {
        this.stats.rejectedCount += 1;
        console.warn('[spark-release-manager] 撤回告知卡片发送失败（权限/限流降级）：', error);
        return false;
      }
    });
  }

  /**
   * 成员侧「本地生成」版本卡片（服务号模型 §20.4.3；档二-4 MVP 降级口径 =
   * 插件加载时补发 + 节流）：发布数据经 org 同步到达每台成员设备后，各设备
   * 上的插件实例从本机数据各自算出卡片写入本机应用会话。幂等去重靠 sdk.data
   * 持久台账（releaseRef = 发布单记录 id 为幂等键），重启/iframe 重建后不重复。
   *
   * 补发范围：状态 = 已发布的发布单（发布卡片）+ 状态 = 已撤回的发布单（撤回
   * 告知卡片；已撤回的不再补发布卡片——诚实呈现以列表/详情为准）。
   * 节流：待补发超过阈值时只补最新一条卡片 + 一条汇总消息，其余直接记账；
   * 遇内核限流/权限拒绝即中止本轮（不重试轰炸），未记账的留待下次加载补齐。
   *
   * 读侧鉴权（fail-closed，spark-announcement 同范式）：append-only 集合挡不住
   * 自制客户端直写伪造 published/retracted 事件——状态派生只认
   * authorizedOperatorRootIds（发布权集合 ∪ 名册管理员）内的操作者事件；
   * 传 null（配置不可得）或空集合时宁可不派生、不推任何卡片（伪造事件
   * 留痕但不驱动卡片），详情页保留原始时间线权威兜底。
   */
  async notifyNewReleases(
    orgId: string,
    releases: ReleaseRecord[],
    events: ReleaseEvent[],
    authorizedOperatorRootIds: ReadonlySet<string> | null
  ): Promise<number> {
    if (!authorizedOperatorRootIds || authorizedOperatorRootIds.size === 0) {
      // fail-closed：发布权配置不可得时宁可不补发（伪造事件不得驱动全员卡片）
      return 0;
    }
    const authorizedEvents = filterAuthorizedReleaseEvents(events, authorizedOperatorRootIds);
    return this.enqueueDelivery(orgId, async () => {
      if (!this.sdk.messages || releases.length === 0) {
        return 0;
      }
      const delivered = await this.loadDeliveredIds(orgId);
      const stateOf = (release: ReleaseRecord): ReleaseState => deriveReleaseState(release.id, authorizedEvents);

      // 撤回告知先行（发布方已撤回的版本，成员应尽快看到告知）
      const pendingRetractions = releases
        .filter((release) => stateOf(release) === 'retracted' && !delivered.has(`${release.id}:retract`))
        .sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      let sent = 0;
      for (const release of pendingRetractions) {
        const event = authorizedEvents
          .filter((item) => item.releaseId === release.id && item.type === 'retracted')
          .sort((a, b) => b.at - a.at || (a.id < b.id ? 1 : -1))[0];
        // 直调内部实现（本批次已在送达串行链上，不能再 enqueue 否则自锁）
        const ok = await this.sendRetractionCardDirect(release, event);
        if (!ok) {
          return sent;
        }
        sent += 1;
      }

      const pending = releases
        .filter((release) => stateOf(release) === 'published' && !delivered.has(release.id))
        .sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      if (pending.length === 0) {
        return sent;
      }

      const batch = selectReleaseBackfillBatch(pending);
      // 历史汇总先行：汇总消息失败则本轮整体中止（什么都不记账），下次加载重试
      if (batch.summarizedCount > 0) {
        const summaryOk = await this.sendHistorySummary(orgId, batch.summarizedCount);
        if (!summaryOk) {
          return sent;
        }
        for (const release of batch.summarized) {
          await this.markDelivered(orgId, release.id, Date.now());
        }
      }

      for (const release of batch.cards) {
        const ok = await this.sendReleaseCardDirect(release);
        if (!ok) {
          // 限流/权限降级：本轮放弃，未记账的发布单下次加载时再补
          break;
        }
        sent += 1;
      }
      return sent;
    });
  }

  /** 发送单条发布卡片并入台账（内部实现，调用方须已持有本空间的送达串行链） */
  private async sendReleaseCardDirect(release: ReleaseRecord): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        {
          summary: buildReleaseCardSummary(release),
          releaseRef: release.id,
          pluginId: release.pluginId,
          version: release.version,
          orgId: release.orgId
        },
        { viewId: 'release-card', data: { releaseId: release.id, orgId: release.orgId } }
      );
      await this.markDelivered(release.orgId, release.id, Date.now());
      this.stats.sentCount += 1;
      return true;
    } catch (error) {
      this.stats.rejectedCount += 1;
      console.warn('[spark-release-manager] 版本卡片发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /** 发送单条撤回告知卡片并入台账（内部实现；event 缺省时摘要按无理由兜底） */
  private async sendRetractionCardDirect(release: ReleaseRecord, event: ReleaseEvent | undefined): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        {
          summary: buildRetractionSummary({ pluginId: release.pluginId, version: release.version, reason: event?.reason }),
          releaseRef: release.id,
          retracted: true,
          pluginId: release.pluginId,
          version: release.version,
          orgId: release.orgId
        },
        { viewId: 'release-card', data: { releaseId: release.id, orgId: release.orgId } }
      );
      await this.markDelivered(release.orgId, `${release.id}:retract`, Date.now());
      this.stats.sentCount += 1;
      return true;
    } catch (error) {
      this.stats.rejectedCount += 1;
      console.warn('[spark-release-manager] 撤回告知卡片发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  /** 「另有 N 个历史版本发布」汇总消息（节流补发防刷屏） */
  private async sendHistorySummary(orgId: string, count: number): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage({ summary: buildReleaseHistorySummary(count), orgId });
      this.stats.sentCount += 1;
      return true;
    } catch (error) {
      this.stats.rejectedCount += 1;
      console.warn('[spark-release-manager] 历史版本汇总消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }

  // ------------------------------------------------------------------
  // 更新对照（sdk.market 只读面；「当前版本 → 最新版本」对照视图，
  // 内置目录签名链路的探测结果原样展示——验收③通路的可见性）
  // ------------------------------------------------------------------

  /** 市场条目只读列表（market 模块缺席返回 null，视图层降级隐藏对照区） */
  async listMarketItems() {
    if (!this.sdk.market) {
      return null;
    }
    return this.sdk.market.list();
  }

  /** 更新探测（内核内置目录签名链路；结果原样展示，不美化） */
  async checkMarketUpdates(pluginId?: string) {
    if (!this.sdk.market) {
      return null;
    }
    return this.sdk.market.checkUpdates(pluginId);
  }
}
