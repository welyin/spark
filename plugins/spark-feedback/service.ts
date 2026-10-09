/**
 * 问题反馈应用（spark-feedback）· 业务服务层（feedback.md v0.2 + 拍板口径）。
 *
 * 职责边界：
 * - MVP 主路径（档一-1）：确保已关注目标议题 → sdk.affairs.create 直建子事务
 *   （type=bug|proposal + refs parent → 目标项目议题，载荷入 extra.feedback）；
 * - 兜底路径：buildExportDraft 预填创世草稿导出，引导到「项目」插件确认提交——
 *   本路径不依赖 sdk.affairs（未就绪时仍可用，验收第 3 条降级口径）；
 * - 环境信息（档二-9）：桥握手 ctx 注入 appVersion/platform/shellVersion（只读、
 *   免权限低敏字段）；ctx 缺省时视图层给手填版本号降级入口；
 * - 默认目标议题冷启动（档三-15）：偏好设置 → 内置 Spark 议题 affairId 常量 →
 *   关注列表选择 / 创世记录粘贴关注；公共目录搜索面 SDK 未暴露（降级，见报告）；
 * - 附件（§3.3）：opt-in 逐件确认 → sdk.content.saveBlob（cid = SHA-256，
 *   保存即声明 provider）→ pinRoot 持有（草稿期 feedback:draft:{id}，提交后
 *   feedback:{childAffairId}）；删除台账条目/草稿时解除根标记走两段式回收；
 * - 每日提交数温馨提示（档三-18）：客户端自律提示，不阻断、不设硬门槛；
 * - 回执卡片（§4）：每次提交至多一条应用消息（message:app 内核限流内自律），
 *   权限被拒/限流降级不阻断主流程；
 * - 能力缺失一律如实降级（affairs 缺 → 仅草稿/导出；content 缺 → 附件区隐藏；
 *   messages 缺 → 无回执卡片），不静默失败、不伪造状态。
 */

import type {
  AffairGenesisInput,
  PluginAffairsAPI,
  PluginContentAPI,
  PluginContext,
  PluginSDK
} from '../../packages/plugin-sdk/src';
import {
  BUILTIN_SPARK_AFFAIR_ID,
  buildEnvironment,
  buildFeedbackPayload,
  countTodaySubmissions,
  isValidAffairId,
  parseDraft,
  parseLedgerEntry,
  parsePrefs,
  statusFromResolutionStates,
  validateFeedbackInput,
  type EnvironmentInfo,
  type FeedbackAttachment,
  type FeedbackDraft,
  type FeedbackInput,
  type FeedbackPrefs,
  type LedgerEntry,
  type LedgerView
} from './model';
import { buildExportDraft, buildFeedbackGenesisInput } from './wire';

/** 本插件用到的 SDK 模块方法核对清单（fail-fast 点名缺失，同 spark-git-repo 纪律） */
export const REQUIRED_AFFAIRS_METHODS = [
  'create', 'follow', 'unfollow', 'listFollowed', 'readLog', 'readResolution'
] as const;
export const REQUIRED_CONTENT_METHODS = ['saveBlob', 'pinRoot', 'unpinRoot'] as const;

export const AFFAIRS_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.affairs（事务模块）——直建子事务回流不可用；仍可保存草稿并手动导出到「项目」插件提交。';
export const CONTENT_MODULE_MISSING =
  '当前宿主未提供可用的 sdk.content（内容面 blob 模块）——附件功能不可用，其余功能不受影响。';

export type PluginCapabilities = {
  /** 直建子事务主路径（缺 → 仅剩草稿 + 手动导出兜底） */
  affairs: boolean;
  /** 附件（opt-in；缺 → 附件区隐藏） */
  content: boolean;
  /** 回执卡片（缺 → 提交后仅台账可见） */
  messages: boolean;
  /** 桥握手 ctx 环境信息可用（缺 → 手填版本号降级，档二-9 注入前口径） */
  environment: boolean;
};

export function hasModule<T extends object>(mod: Partial<T> | undefined, methods: readonly string[]): mod is T {
  return Boolean(mod && methods.every((method) => typeof (mod as Record<string, unknown>)[method] === 'function'));
}

export function probeCapabilities(sdk: PluginSDK, ctx?: Partial<PluginContext> | null): PluginCapabilities {
  return {
    affairs: hasModule<PluginAffairsAPI>(sdk.affairs, REQUIRED_AFFAIRS_METHODS),
    content: hasModule<PluginContentAPI>(sdk.content, REQUIRED_CONTENT_METHODS),
    messages: typeof sdk.messages?.sendAppMessage === 'function',
    environment: Boolean(ctx?.appVersion || ctx?.platform || ctx?.shellVersion)
  };
}

// ------------------------------------------------------------------
// 集合声明（feedback.md §3.2）
// ------------------------------------------------------------------

/** 反馈草稿：lww-record + local（草稿是设备现场，多设备不同步） */
const DRAFTS_COLLECTION = 'spark-feedback:drafts';
/**
 * 反馈台账：lww-record + sync（「我提交过什么」个人空间自设备间可见）。
 * 口径（评审修复拍板）：台账只是个人侧指针，真正的留痕在事务面子事务
 * （数据主权），append-only 对个人台账无审计价值且内核拒绝其删除——
 * lww 删除即墓碑传播，「删除记录」得以成立。
 */
const LEDGER_COLLECTION = 'spark-feedback:ledger';
/** 偏好配置：lww-record + sync（默认目标议题 / 环境信息附带开关） */
const PREFS_COLLECTION = 'spark-feedback:prefs';
const PREFS_KEY = 'prefs';

/** 目标议题解析结果（档三-15 三路兜底） */
export type TargetResolution = {
  affairId: string;
  source: 'prefs' | 'builtin';
} | null;

function nonNull<T>(value: T | null): value is T {
  return value !== null;
}

export class FeedbackService {
  private readonly affairs: PluginAffairsAPI | null;
  private readonly content: PluginContentAPI | null;
  private collectionsReady = false;

  constructor(
    private readonly sdk: PluginSDK,
    private readonly ctx?: Partial<PluginContext> | null
  ) {
    this.affairs = hasModule<PluginAffairsAPI>(sdk.affairs, REQUIRED_AFFAIRS_METHODS) ? sdk.affairs : null;
    this.content = hasModule<PluginContentAPI>(sdk.content, REQUIRED_CONTENT_METHODS) ? sdk.content : null;
  }

  static capabilities(sdk: PluginSDK, ctx?: Partial<PluginContext> | null): PluginCapabilities {
    return probeCapabilities(sdk, ctx);
  }

  private requireAffairs(): PluginAffairsAPI {
    if (!this.affairs) {
      throw new Error(AFFAIRS_MODULE_MISSING);
    }
    return this.affairs;
  }

  private requireContent(): PluginContentAPI {
    if (!this.content) {
      throw new Error(CONTENT_MODULE_MISSING);
    }
    return this.content;
  }

  private async ensureCollections(): Promise<void> {
    if (this.collectionsReady) {
      return;
    }
    await this.sdk.data.declareCollection({ name: DRAFTS_COLLECTION, merge: 'lww-record', scope: 'local' });
    await this.sdk.data.declareCollection({ name: LEDGER_COLLECTION, merge: 'lww-record' });
    await this.sdk.data.declareCollection({ name: PREFS_COLLECTION, merge: 'lww-record' });
    this.collectionsReady = true;
  }

  // ------------------------------------------------------------------
  // 环境信息（档二-9：ctx 注入只读；缺省字段不编造）
  // ------------------------------------------------------------------

  /** 预览/提交共用的环境信息组装（ctx 缺什么省什么；spaceKind 缺省按 personal） */
  buildEnvironmentPreview(reportedVersion?: string): EnvironmentInfo {
    const space = this.ctx?.space ?? { type: 'personal' as const, id: 'personal' };
    return buildEnvironment(
      {
        space,
        ...(typeof this.ctx?.appVersion === 'string' ? { appVersion: this.ctx.appVersion } : {}),
        ...(typeof this.ctx?.platform === 'string' ? { platform: this.ctx.platform } : {}),
        ...(typeof this.ctx?.shellVersion === 'string' ? { shellVersion: this.ctx.shellVersion } : {})
      },
      reportedVersion
    );
  }

  // ------------------------------------------------------------------
  // 偏好配置
  // ------------------------------------------------------------------

  async getPrefs(): Promise<FeedbackPrefs> {
    await this.ensureCollections();
    return parsePrefs(await this.sdk.data.get(PREFS_COLLECTION, PREFS_KEY));
  }

  async savePrefs(prefs: FeedbackPrefs): Promise<void> {
    await this.ensureCollections();
    if (prefs.defaultTargetAffairId !== undefined && prefs.defaultTargetAffairId !== ''
      && !isValidAffairId(prefs.defaultTargetAffairId)) {
      throw new Error('默认目标议题 affairId 形状非法（须为 64 位小写 hex）');
    }
    await this.sdk.data.save(PREFS_COLLECTION, PREFS_KEY, { ...prefs });
  }

  // ------------------------------------------------------------------
  // 目标议题解析（档三-15：偏好 → 内置常量 → 用户选择/邀请关注）
  // ------------------------------------------------------------------

  /**
   * 解析默认目标议题：偏好设置优先（须本机已关注）；其次内置 Spark 议题常量
   * （未签发占位期间跳过）；公共目录搜索面 SDK 未暴露（降级：用户从关注列表
   * 选择或粘贴创世记录关注）。均不可达 → null（视图层给补投/草稿引导）。
   */
  async resolveTarget(): Promise<TargetResolution> {
    const affairs = this.requireAffairs();
    const followed = new Set(await affairs.listFollowed());
    const prefs = await this.getPrefs();
    if (prefs.defaultTargetAffairId && followed.has(prefs.defaultTargetAffairId)) {
      return { affairId: prefs.defaultTargetAffairId, source: 'prefs' };
    }
    if (isValidAffairId(BUILTIN_SPARK_AFFAIR_ID) && followed.has(BUILTIN_SPARK_AFFAIR_ID)) {
      return { affairId: BUILTIN_SPARK_AFFAIR_ID, source: 'builtin' };
    }
    return null;
  }

  /** 本机关注的事务清单（目标选择器用；创世未同步的跳过不编造） */
  async listFollowedTopics(): Promise<Array<{ affairId: string; title: string; type: string }>> {
    const affairs = this.requireAffairs();
    const ids = await affairs.listFollowed();
    const items = await Promise.all(
      ids.map(async (affairId) => {
        const log = await affairs.readLog(affairId);
        const genesis = log.genesis as Record<string, unknown> | null;
        if (!genesis || typeof genesis.title !== 'string') {
          return null;
        }
        return { affairId, title: genesis.title, type: typeof genesis.type === 'string' ? genesis.type : '' };
      })
    );
    return items.filter(nonNull);
  }

  /** 目标议题元数据（公开性明示依据）；不可达 → null（允许先存草稿后补投） */
  async targetMeta(affairId: string): Promise<{ title: string; isPublic: boolean } | null> {
    const affairs = this.requireAffairs();
    try {
      const log = await affairs.readLog(affairId);
      const genesis = log.genesis as Record<string, unknown> | null;
      if (!genesis) {
        return null;
      }
      return {
        title: typeof genesis.title === 'string' ? genesis.title : affairId.slice(0, 12),
        isPublic: genesis.publish === true
      };
    } catch {
      return null;
    }
  }

  /** 关注已有事务（创世记录原文粘贴——邀请链接/转发路径；内核全链校验 + affairId 自认证复算） */
  async followGenesis(genesis: unknown): Promise<string> {
    const affairs = this.requireAffairs();
    if (typeof genesis !== 'object' || genesis === null || Array.isArray(genesis)) {
      throw new Error('创世记录必须是 JSON 对象');
    }
    return affairs.follow(genesis as Record<string, unknown>);
  }

  // ------------------------------------------------------------------
  // 附件（opt-in 逐件确认后上传；pinRoot 持有纪律 §3.3）
  // ------------------------------------------------------------------

  /**
   * 上传附件：saveBlob（cid = SHA-256，保存即声明 provider）→ pinRoot 草稿根
   * （提交成功后将根迁移到台账条目）。调用前视图层须逐件明示隐私后果
   * （随公开议题扩散不可收回；日志类附件可能含路径/联系人信息）。
   */
  async uploadAttachment(draftId: string, input: { dataBase64: string; name: string; mime?: string }): Promise<FeedbackAttachment> {
    const content = this.requireContent();
    const info = await content.saveBlob(input.dataBase64);
    await content.pinRoot(info.cid, `feedback:draft:${draftId}`);
    return { cid: info.cid, name: input.name, size: info.size, ...(input.mime ? { mime: input.mime } : {}) };
  }

  /** 移除草稿期附件（解除草稿根标记，无根 blob 走两段式回收） */
  async discardAttachment(draftId: string, cid: string): Promise<void> {
    if (!this.content) {
      return;
    }
    try {
      await this.content.unpinRoot(cid, `feedback:draft:${draftId}`);
    } catch {
      // 解除失败不阻断（无根判定以内核为准，宽限期后仍走两段式回收）
    }
  }

  /**
   * 草稿恢复时为当前表单会话补挂附件根标记（根标记按 cid × root 计：
   * 原草稿根的持有不变，新会话根在提交/取消勾选时各自解除，互不误伤）。
   */
  async retainDraftAttachments(draftId: string, attachments: FeedbackAttachment[]): Promise<void> {
    if (!this.content) {
      return;
    }
    for (const attachment of attachments) {
      try {
        await this.content.pinRoot(attachment.cid, `feedback:draft:${draftId}`);
      } catch {
        // 尽力而为
      }
    }
  }

  // ------------------------------------------------------------------
  // 草稿（local 集合；目标议题元数据不可达时先存草稿后补投，验收第 3 条）
  // ------------------------------------------------------------------

  async saveDraft(draft: Omit<FeedbackDraft, 'savedAt'>): Promise<void> {
    await this.ensureCollections();
    await this.sdk.data.save(DRAFTS_COLLECTION, draft.id, { ...draft, savedAt: Date.now() });
  }

  async listDrafts(): Promise<FeedbackDraft[]> {
    await this.ensureCollections();
    const { items } = await this.sdk.data.query(DRAFTS_COLLECTION, { limit: 200 });
    return items
      .map((item) => parseDraft(item.key, item.value))
      .filter(nonNull)
      .sort((a, b) => b.savedAt - a.savedAt);
  }

  async deleteDraft(draft: FeedbackDraft): Promise<void> {
    await this.ensureCollections();
    await this.sdk.data.delete(DRAFTS_COLLECTION, draft.id);
    for (const attachment of draft.attachments) {
      await this.discardAttachment(draft.id, attachment.cid);
    }
  }

  // ------------------------------------------------------------------
  // 台账（lww-record sync；状态呈现按本地副本所见推导，不回写不编造）
  // ------------------------------------------------------------------

  async listLedger(): Promise<LedgerEntry[]> {
    await this.ensureCollections();
    const { items } = await this.sdk.data.query(LEDGER_COLLECTION, { limit: 500 });
    return items
      .map((item) => parseLedgerEntry(item.value))
      .filter(nonNull)
      .sort((a, b) => b.submittedAt - a.submittedAt);
  }

  /** 台账 + 各子事务本地副本所见的决议状态（readLog/readResolution 失败如实标注） */
  async listLedgerView(): Promise<LedgerView[]> {
    const entries = await this.listLedger();
    if (!this.affairs) {
      return entries.map((entry) => ({ ...entry, status: 'unavailable' as const }));
    }
    return Promise.all(
      entries.map(async (entry) => {
        try {
          const log = await this.affairs!.readLog(entry.childAffairId);
          if (!log.genesis) {
            return { ...entry, status: 'unavailable' as const };
          }
          const resolutions = await this.affairs!.readResolution(entry.childAffairId);
          return {
            ...entry,
            status: statusFromResolutionStates(resolutions.resolutions.map((r) => r.state))
          };
        } catch {
          return { ...entry, status: 'unavailable' as const };
        }
      })
    );
  }

  /** 今日已提交数（每日温馨提示依据；本地日历日口径） */
  async countToday(nowMs = Date.now()): Promise<number> {
    return countTodaySubmissions(await this.listLedger(), nowMs);
  }

  /** 删除台账条目（个人侧指针；子事务数据属于议题不受影响）→ 附件尽力解除 pinRoot */
  async removeLedgerEntry(entry: LedgerEntry): Promise<void> {
    await this.ensureCollections();
    // delete 失败（合并规则/同步面异常）不阻断 pinRoot 解除——两条清理路径各自尽力
    let deleted = false;
    let deleteError: Error | null = null;
    try {
      await this.sdk.data.delete(LEDGER_COLLECTION, entry.id);
      deleted = true;
    } catch (error) {
      deleteError = error as Error;
    }
    if (this.content && this.affairs) {
      try {
        const log = await this.affairs.readLog(entry.childAffairId);
        const feedback = (log.genesis as Record<string, unknown> | null)?.feedback as
          | { attachments?: Array<{ cid?: unknown }> }
          | undefined;
        for (const attachment of feedback?.attachments ?? []) {
          if (typeof attachment.cid === 'string') {
            await this.content.unpinRoot(attachment.cid, `feedback:${entry.childAffairId}`);
          }
        }
      } catch {
        // 子事务副本不可读 / 解除失败均不阻断（回收以内核无根判定为准）
      }
    }
    if (!deleted) {
      throw new Error(`台账删除失败：${deleteError?.message ?? '未知原因'}`);
    }
  }

  // ------------------------------------------------------------------
  // 提交（MVP 主路径，档一-1）
  // ------------------------------------------------------------------

  /**
   * 提交反馈：输入校验 → 目标议题已关注断言 → 公开性纵深校验（目标议题公开
   * 时须携 confirmedPublic 确认，服务层复核不依赖视图层纪律）→
   * sdk.affairs.create 直建子事务（type + refs parent + extra.feedback 载荷，
   * SDK 承载签名与内核全链校验）→ 附件 pinRoot 迁移到台账根 → 台账登记 →
   * 回执卡片（降级不阻断）。
   */
  async submit(
    input: FeedbackInput,
    targetAffairId: string,
    options: { draftId?: string; confirmedPublic?: boolean } = {}
  ): Promise<{ entry: LedgerEntry; cardSent: boolean }> {
    const { draftId, confirmedPublic } = options;
    const verdict = validateFeedbackInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    if (!isValidAffairId(targetAffairId)) {
      throw new Error('目标议题 affairId 形状非法（须为 64 位小写 hex）');
    }
    const affairs = this.requireAffairs();
    const followed = await affairs.listFollowed();
    if (!followed.includes(targetAffairId)) {
      throw new Error('尚未关注目标议题——请先在「项目」插件或下方入口关注后再提交（关注即副本语义）');
    }
    // 公开性纵深校验（验收第 1 条）：目标议题元数据可读且公开时，未逐次确认即拒；
    // 元数据不可达按「未知」处理（诚实边界——不可达场景走草稿补投，不由本校验拦截）
    const meta = await this.targetMeta(targetAffairId);
    if (meta?.isPublic && confirmedPublic !== true) {
      throw new Error(`目标议题「${meta.title}」为公开议题：反馈内容（含附件）将公开扩散且不可撤回，请先逐次确认再提交`);
    }
    const environment = input.includeEnvironment
      ? this.buildEnvironmentPreview(input.reportedVersion)
      : null;
    const payload = buildFeedbackPayload(input, environment);
    const genesisInput: AffairGenesisInput = buildFeedbackGenesisInput({
      type: input.type,
      title: input.title,
      targetAffairId,
      payload
    });
    const { affairId } = await affairs.create(genesisInput);

    // 附件 GC 根迁移：草稿根 → 台账根（持有期间 pinRoot，§3.3）
    if (this.content) {
      for (const attachment of input.attachments) {
        try {
          await this.content.pinRoot(attachment.cid, `feedback:${affairId}`);
          if (draftId) {
            await this.content.unpinRoot(attachment.cid, `feedback:draft:${draftId}`);
          }
        } catch {
          // 根标记迁移失败不阻断主流程（回收判定以内核为准）
        }
      }
    }

    const entry: LedgerEntry = {
      id: `fb-${Date.now().toString(36)}-${affairId.slice(0, 12)}`,
      targetAffairId,
      childAffairId: affairId,
      type: input.type,
      title: input.title.trim(),
      submittedAt: Date.now()
    };
    await this.ensureCollections();
    await this.sdk.data.save(LEDGER_COLLECTION, entry.id, { ...entry });
    if (draftId) {
      await this.sdk.data.delete(DRAFTS_COLLECTION, draftId);
    }
    const cardSent = await this.notifyReceipt(entry);
    return { entry, cardSent };
  }

  // ------------------------------------------------------------------
  // 手动兜底导出（档一-1 兜底；不依赖 sdk.affairs）
  // ------------------------------------------------------------------

  /**
   * 生成预填创世草稿（标题/正文/refs 均已填好），引导用户到「项目」插件内
   * 确认提交。目标 affairId 须用户提供（粘贴）；形状非法即拒。
   */
  buildExport(input: FeedbackInput, targetAffairId: string): string {
    const verdict = validateFeedbackInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    if (!isValidAffairId(targetAffairId)) {
      throw new Error('目标议题 affairId 形状非法（须为 64 位小写 hex）');
    }
    const environment = input.includeEnvironment
      ? this.buildEnvironmentPreview(input.reportedVersion)
      : null;
    const genesisInput = buildFeedbackGenesisInput({
      type: input.type,
      title: input.title,
      targetAffairId,
      payload: buildFeedbackPayload(input, environment)
    });
    return buildExportDraft(genesisInput);
  }

  // ------------------------------------------------------------------
  // 回执卡片（§4 限流自律：每次提交至多一条；失败降级不阻断）
  // ------------------------------------------------------------------

  /** 提交回执（message:app；summary 遵守 §20.2 不变量：非空、trim ≤200、自含完整语义） */
  private async notifyReceipt(entry: LedgerEntry): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    const typeLabel = entry.type === 'bug' ? '缺陷' : '建议';
    const summary = `[反馈已提交] ${typeLabel}「${entry.title}」已回流为目标项目议题的子事务（${entry.childAffairId.slice(0, 12)}…），处理进展在「我的反馈」中查看。`
      .slice(0, 200)
      .trimEnd();
    try {
      await this.sdk.messages.sendAppMessage(
        { summary, childAffairId: entry.childAffairId, targetAffairId: entry.targetAffairId },
        {
          viewId: 'feedback-card',
          data: {
            type: entry.type,
            title: entry.title,
            childAffairId: entry.childAffairId,
            targetAffairId: entry.targetAffairId
          }
        }
      );
      return true;
    } catch (error) {
      console.warn('[spark-feedback] 回执卡片发送失败（权限/限流降级）：', error);
      return false;
    }
  }
}
