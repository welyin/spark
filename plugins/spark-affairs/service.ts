/**
 * 公共议题客户端（spark-affairs）· 业务服务层。
 *
 * 职责边界（community-affairs.md §7，SDK 面见 plugin-sdk PluginAffairsAPI）：
 * - 事务容器全部走 sdk.affairs：创建 = sdk.affairs.create（SDK 承载创世
 *   线形构造 + 插件域身份签名 + follow 全链校验；refs 走 §10 类型化暴露，
 *   不再硬编码空数组）→ submitOp 把议题说明作为首条内容操作入日志；
 *   决议/阶梯由内核从日志 + 存证链确定性推导；
 * - 变更订阅走 sdk.affairs.onChange（AffairChanged 事件）——视图不再靠
 *   手动刷新兜底（变更通知非可靠队列，收到后重读 readLog 收敛）；
 * - 本插件只承载客户端解释层：表单校验、规则文档映射与操作线形构造
 *   （wire.ts）、视图映射（model.ts）、应用会话通知（message:app）；
 * - 通知是增强能力：授权被拒/限流时降级不阻断主流程；但操作的协议签名
 *   是内核入站校验的硬要求（无 sig 必被拒），签名被拒 = 提交失败，不降级。
 *
 * 签名主体诚实口径（评审阻塞 3 的降级处理）：平台 SDK 只有 identity.sign
 * （插件域身份签名，无个人身份签名面），故创世 initiator 与操作 actor 均为
 * 本插件域身份（plugin:spark-affairs 派生钥匙）；UI 展示的身份 id 不代表
 * 发起人/操作者的个人身份。平台层拍板个人身份签名路径后应迁移。
 */
import type { AffairChangeEvent, AffairGenesisInput, AffairOpStatus, PluginAffairsAPI, PluginSDK } from '../../packages/plugin-sdk/src';
import { hasAffairsModule, requireAffairsModule } from './sdk-affairs';
import {
  AFFAIR_TYPE,
  buildOpDraft,
  buildRulesDoc,
  deriveIdentity,
  signPayload,
  type AffairActor
} from './wire';
import {
  buildAffairSummary,
  readGenesisMeta,
  rulesFromGenesis,
  sortOperations,
  toAffairOperation,
  toExecStateViews,
  toLadderEntryView,
  toLadderState,
  toOrgEffectsView,
  toResolutionView,
  toRulesChainView,
  validateAffairDraft,
  validateCommentText,
  validateContributionText,
  type AffairCreateInput,
  type AffairDetail,
  type AffairListItem,
  type AffairOperation,
  type AffairOperationInput,
  type AffairResolutionView,
  type ExecStateView,
  type LadderEntryView,
  type LadderState,
  type OrgEffectApplyReport,
  type OrgEffectView,
  type OrgEffectsView,
  type RulesChainView
} from './model';

function nonNull<T>(value: T | null): value is T {
  return value !== null;
}

export class AffairsService {
  private readonly affairs: PluginAffairsAPI;
  /** 本插件域身份 actor（首次写操作时经 identity.sign 取回公钥后缓存） */
  private actor: AffairActor | null = null;

  constructor(private readonly sdk: PluginSDK) {
    this.affairs = requireAffairsModule(sdk);
  }

  /** 宿主是否提供可用的 sdk.affairs（模块存在且方法齐全；否则视图降级为提示页） */
  static isAvailable(sdk: PluginSDK): boolean {
    return hasAffairsModule(sdk);
  }

  /** 本机在本事务中的操作者身份（插件域身份 id；尚无写操作时为 null） */
  get viewerIdentity(): string | null {
    return this.actor?.identity ?? null;
  }

  /**
   * 取本插件域身份 actor。SDK 只有签名面、没有公钥查询面，故以一次签名调用
   * 取回 DomainSignature.publicKey 并推导身份 id（§2.2 自包含绑定），缓存复用；
   * 之后每条创世/操作只需各一次签名。
   */
  private async ensureActor(): Promise<AffairActor> {
    if (this.actor) {
      return this.actor;
    }
    const probe = await this.sdk.identity.sign('spark-affairs:actor-probe');
    this.actor = {
      kind: 'person',
      identity: deriveIdentity(probe.publicKey),
      publicKey: probe.publicKey
    };
    return this.actor;
  }

  /**
   * 给协议记录补签名：载荷 = canonical(记录剔除 sig)（community README 总约），
   * identity:sign 以插件域身份签名。签名是内核入站校验的硬要求（缺 sig 必被拒），
   * 用户拒绝授权时直接失败上抛——不产出无签名记录（与通知类降级不同）。
   */
  private async signRecord(record: Record<string, unknown>): Promise<Record<string, unknown>> {
    const signed = await this.sdk.identity.sign(signPayload(record));
    return { ...record, sig: signed.signature };
  }

  /** 本机关注的议题列表（落地 SDK 无 indexer 目录面，事务墙 = 关注簿记 + readLog 元数据） */
  async listFollowed(): Promise<AffairListItem[]> {
    const ids = await this.affairs.listFollowed();
    const items = await Promise.all(
      ids.map(async (affairId) => {
        const log = await this.affairs.readLog(affairId);
        const meta = readGenesisMeta(log.genesis);
        if (!meta) {
          // 创世未同步到位（复制未收敛）：跳过而非编造占位元数据
          return null;
        }
        return {
          affairId,
          ...meta,
          following: log.followedAt !== null,
          operationCount: log.ops.length
        };
      })
    );
    return items.filter(nonNull);
  }

  async getDetail(affairId: string): Promise<AffairDetail> {
    const [log, resolutions] = await Promise.all([
      this.affairs.readLog(affairId),
      this.affairs.readResolution(affairId)
    ]);
    const meta = readGenesisMeta(log.genesis);
    if (!meta) {
      throw new Error(`议题 ${affairId.slice(0, 12)}… 的创世记录尚未同步到本机（等待复制收敛）。`);
    }
    const closed = resolutions.resolutions
      .map(toResolutionView)
      .filter(nonNull)
      .some((resolution) => resolution.state === 'effective' || resolution.state === 'vetoed');
    return {
      affairId,
      ...meta,
      following: log.followedAt !== null,
      operationCount: log.ops.length,
      rules: rulesFromGenesis(log.genesis),
      closed
    };
  }

  /** 操作时间线（展示序：声明时刻 + opHash tie-break；纪律见 model.sortOperations） */
  async listOperations(affairId: string): Promise<AffairOperation[]> {
    const log = await this.affairs.readLog(affairId);
    return sortOperations(log.ops.map((entry) => toAffairOperation(entry.opHash, entry.op)).filter(nonNull));
  }

  /** 阶梯名册（内核从已接受操作集合 + 链上锚定时刻确定性推导，插件伪造不了） */
  async getLadderRoster(affairId: string): Promise<{ entries: LadderEntryView[]; voters: string[] }> {
    const status = await this.affairs.ladderStatus(affairId);
    return {
      entries: status.entries.map((entry) => toLadderEntryView(entry, status.nowMs)).filter(nonNull),
      voters: status.voters
    };
  }

  /** 「我的」阶梯状态（本插件域身份在名册中的条目；无写操作历史/不在名册时为 null） */
  async getMyLadderState(affairId: string): Promise<LadderState | null> {
    if (!this.actor) {
      return null;
    }
    const status = await this.affairs.ladderStatus(affairId);
    const mine = status.entries.find((entry) => entry.identity === this.actor?.identity);
    return mine ? toLadderState(mine, status.nowMs) : null;
  }

  /** 决议列表（公示期状态由内核按链上锚定时刻推导；未锚定如实标 unanchored） */
  async listResolutions(affairId: string): Promise<AffairResolutionView[]> {
    const resolutions = await this.affairs.readResolution(affairId);
    return resolutions.resolutions.map(toResolutionView).filter(nonNull);
  }

  /** 规则版本链（readRules：现行版本 + 未生效条目归宿；结构不符返回 null 而非编造） */
  async getRulesChain(affairId: string): Promise<RulesChainView | null> {
    return toRulesChainView(await this.affairs.readRules(affairId));
  }

  /** 执行状态（readExec；非执行型事务 exec == null → 空列表，决议即终态） */
  async getExecStates(affairId: string): Promise<ExecStateView[]> {
    return toExecStateViews(await this.affairs.readExec(affairId));
  }

  /** 组织效力读出口（orgEffects：逐声明 × 逐决议判定 + 回执状态标注） */
  async getOrgEffects(orgId: string, affairId: string): Promise<OrgEffectsView | null> {
    return toOrgEffectsView(await this.affairs.orgEffects(orgId, affairId));
  }

  /**
   * 决议效力应用编排（org-genesis §6 消费侧参考实现；next.md 事项 6）。
   *
   * 内核口径（affair_ops.rs 头注/apply 门面 doc）：名册/策略内容的实际变更
   * 不在内核（决议 tally 是插件语义，内核只承诺字节）——内容应用归插件/组织
   * 侧，回执（org:effectrcpt:）即「该决议对该组织此 scope 已生效」的机器
   * 可读凭据。故编排纪律：
   *
   * 1. orgEffects 取待应用事件（outcome=apply 且回执 unrecorded）；
   * 2. 逐条按 scope 做内容应用：
   *    - `policy`：决议 result 即声明式策略文档草稿 → sdk.policy.submitDraft
   *      （内核结构+引擎校验兜底；发布合入 org:policydoc: 归管理员授权面
   *      policy.publish，本编排不自动发布）。sdk.policy 缺席或 policy:write
   *      被拒 → 该条 unapplied；
   *    - `roster` / `create` / `budget:*`：插件 SDK 无组织名册/预算写面
   *      （组织侧工具域），参考实现不伪造应用——如实 unapplied；
   * 3. 全部待应用事件应用成功 → applyOrgEffects 写回执（内核幂等、同 scope
   *    多决议只跟踪最新——链上时间 LWW）；任一 unapplied → 不写任何回执
   *    （fail-closed：未应用不得出具「已生效」凭据）。
   */
  async applyOrgEffects(orgId: string, affairId: string): Promise<OrgEffectApplyReport> {
    const raw = await this.affairs.orgEffects(orgId, affairId);
    const view = toOrgEffectsView(raw);
    if (!view) {
      throw new Error('sdk.affairs.orgEffects 返回形状不符（无法判定待应用事件，不写回执）');
    }
    const pending = view.effects.filter(
      (effect) => effect.outcome === 'apply' && effect.receiptState === 'unrecorded'
    );
    if (pending.length === 0) {
      return { applied: [], unapplied: [], receiptActions: null };
    }
    const resolutions = await this.affairs.readResolution(affairId);
    const applied: string[] = [];
    const unapplied: OrgEffectApplyReport['unapplied'] = [];
    for (const effect of pending) {
      const reason = await this.applyEffectContent(effect, resolutions.resolutions);
      if (reason === null) {
        applied.push(effect.scope);
      } else {
        unapplied.push({ scope: effect.scope, resolutionOpHash: effect.resolutionOpHash, reason });
      }
    }
    if (unapplied.length > 0) {
      return { applied, unapplied, receiptActions: null };
    }
    const result = await this.affairs.applyOrgEffects(orgId, affairId);
    return {
      applied,
      unapplied: [],
      receiptActions: result.actions.map((action) => ({
        scope: action.scope,
        resolutionOpHash: action.resolutionOpHash,
        action: action.action
      }))
    };
  }

  /**
   * 单条待应用事件的内容应用（applyOrgEffects 的第 2 步）。返回 null = 应用
   * 成功；否则为未应用原因（稳定文案，上报进 OrgEffectApplyReport.unapplied）。
   */
  private async applyEffectContent(
    effect: OrgEffectView,
    resolutions: Array<{ opHash: string; result: unknown }>
  ): Promise<string | null> {
    if (effect.scope !== 'policy') {
      // roster/create/budget:*：插件 SDK 无对应组织写面，内容应用归组织侧工具
      return `scope ${effect.scope} 无插件侧写面（名册/预算内容应用归组织侧工具）`;
    }
    const resolution = resolutions.find((item) => item.opHash === effect.resolutionOpHash);
    const result = resolution?.result;
    if (typeof result !== 'object' || result === null || Array.isArray(result)) {
      return '决议 result 不是策略文档形状（policy 效力要求决议结果即声明式策略草稿）';
    }
    if (!this.sdk.policy) {
      return '宿主未提供 sdk.policy（policy 效力应用需要 policy:write 授权）';
    }
    try {
      await this.sdk.policy.submitDraft(result as Record<string, unknown>);
      return null;
    } catch (error) {
      return `策略草稿提交失败：${(error as Error).message}`;
    }
  }

  /**
   * 发起议题（affairs:write）：sdk.affairs.create 承载创世线形构造 + 签名 +
   * follow（内核全链校验，affairId 自认证复算；refs 真实携带 §10 枚举）→
   * submitOp 把议题说明作为首条内容操作入日志（关注者副本的时间线不为空）。
   */
  async createAffair(input: AffairCreateInput): Promise<{ affairId: string }> {
    const verdict = validateAffairDraft(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    const genesisInput: AffairGenesisInput = {
      type: AFFAIR_TYPE,
      title: input.title,
      summary: input.summary,
      tags: input.tags,
      refs: input.refs,
      rules: buildRulesDoc(input.rules),
      extra: {
        // initialVoters 非空才携带（空集合 = 内核缺省仅发起人，缺键不承诺）
        ...(input.rules.initialVoters.length > 0 ? { initialVoters: input.rules.initialVoters } : {}),
        // 插件语义字段（§六：仅检索聚合用，不代表机构认可）
        ...(input.regionCode?.trim() ? { regionCode: input.regionCode.trim() } : {})
      }
    };
    const { affairId } = await this.affairs.create(genesisInput);
    await this.submitOperation(affairId, { kind: 'comment', payload: { text: input.summary.trim() } });
    return { affairId };
  }

  /**
   * 订阅本机事务副本变更（sdk.affairs.onChange）：关注/取关/提交/复制合入
   * 后由内核事件驱动视图刷新——替代手动刷新兜底。变更通知非可靠队列，
   * handler 内应重读（readLog/listFollowed）收敛而非增量套用。
   */
  subscribeChanges(handler: (event: AffairChangeEvent) => void): Promise<void> {
    return this.affairs.onChange(handler);
  }

  /**
   * 关注已有议题（affairs:write）：sdk.affairs.follow 只收创世记录原文——
   * affairId 由创世自认证复算，不接受自报 id。非法记录由内核校验链拒绝并上抛。
   */
  async followGenesis(genesis: unknown): Promise<string> {
    if (typeof genesis !== 'object' || genesis === null || Array.isArray(genesis)) {
      throw new Error('创世记录必须是 JSON 对象');
    }
    return this.affairs.follow(genesis as Record<string, unknown>);
  }

  /** 取关（affairs:write）：只删关注簿记，已复制数据保留 */
  unfollow(affairId: string): Promise<void> {
    return this.affairs.unfollow(affairId);
  }

  /** 追加一条内容操作（affairs:write）：因果见证取本地观察到的 DAG 头（首条 = affairId） */
  private async submitOperation(
    affairId: string,
    operation: AffairOperationInput
  ): Promise<{ opHash: string; status: AffairOpStatus }> {
    const actor = await this.ensureActor();
    const log = await this.affairs.readLog(affairId);
    const draft = buildOpDraft(affairId, actor, operation.kind, operation.payload, log.heads, Date.now());
    const result = await this.affairs.submitOp(await this.signRecord(draft));
    return { opHash: result.opHash, status: result.status };
  }

  /** 提交贡献（正式贡献；采纳走「延迟生效+阈值否决」集体决策，由内核推导） */
  async submitContribution(affairId: string, text: string): Promise<{ opHash: string; status: AffairOpStatus }> {
    const verdict = validateContributionText(text);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    return this.submitOperation(affairId, { kind: 'contribution', payload: { text: text.trim() } });
  }

  /** 投票（插件语义载荷；一人一票不加权，identityMode 由用户自选） */
  async submitVote(
    affairId: string,
    targetOpHash: string,
    choice: 'for' | 'against' | 'abstain',
    identityMode: 'contextual' | 'public'
  ): Promise<{ opHash: string; status: AffairOpStatus }> {
    return this.submitOperation(affairId, {
      kind: 'vote',
      payload: { targetOpHash, choice, identityMode }
    });
  }

  /** 评论（观察层零门槛，垃圾评论靠客户端过滤与主持折叠，不进协议） */
  async submitComment(affairId: string, text: string): Promise<{ opHash: string; status: AffairOpStatus }> {
    const verdict = validateCommentText(text);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    return this.submitOperation(affairId, { kind: 'comment', payload: { text: text.trim() } });
  }

  /**
   * 发起议题后的应用会话通知（message:app，服务号模型 §20.4.3：
   * 本地生成、本地消费，只到发起人本机；卡片 data 只放 affairId 引用）。
   * 权限被拒/限流降级返回 false，成功文案不得声称「他人已收到」。
   */
  async notifyNewAffair(title: string, affairId: string): Promise<boolean> {
    if (!this.sdk.messages) {
      return false;
    }
    try {
      await this.sdk.messages.sendAppMessage(
        { summary: buildAffairSummary(title), affairId },
        { viewId: 'affair-card', data: { affairId } }
      );
      return true;
    } catch (error) {
      console.warn('[spark-affairs] 应用消息发送失败（权限/限流降级）：', error);
      return false;
    }
  }
}
