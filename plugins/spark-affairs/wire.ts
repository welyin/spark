/**
 * 公共议题客户端（spark-affairs）· 协议线形构造（wiki/protocol/community/affair.md §2/§3）。
 *
 * 分层（SDK 四项自承落地后的口径）：
 * - 通用协议线形（canonical/哈希/身份推导/创世草稿/refs 类型）已上收
 *   packages/plugin-sdk/src/affair-wire.ts——创建事务走 sdk.affairs.create
 *   （SDK 承载创世构造 + 域身份签名 + follow），本插件不再自有创世构造；
 * - 本文件只保留插件语义层：事务类型标识、客户端规则草稿 → 协议规则文档
 *   的映射（buildRulesDoc，含内核可判定的 op-count 关闭条件）、内容操作
 *   条目构造（buildOpDraft，opType=content 的插件语义载荷内核不解释）、
 *   决议操作条目构造（buildResolutionDraft，opType=resolution，§6.1）；
 * - 操作签名主体仍是本插件域身份（SDK 只有域身份签名面）——协议 actor 为
 *   插件域身份（kind:person 线形），不代表操作者个人身份（诚实口径）。
 *
 * 本文件不依赖 SDK 运行时/Vue（affair-wire 同为纯函数模块），全部可单测。
 */

import type { AffairOperationKind, AffairRules, CommentPayload, ContributionPayload, ResolutionPlan, VotePayload } from './model';
import type { AffairActor } from '../../packages/plugin-sdk/src/affair-wire';

// 通用线形助手经本文件 re-export（插件内既有 import 路径不变；唯一实现
// 在 SDK affair-wire，与内核 canonical.rs 同向量）
export {
  base64Decode,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload
} from '../../packages/plugin-sdk/src/affair-wire';
export type { AffairActor } from '../../packages/plugin-sdk/src/affair-wire';

/** 本插件的事务类型标识（插件命名空间，内核不解释；affair.md §2.1 形状） */
export const AFFAIR_TYPE = 'spark-affairs:topic';

/** 公示期协议下限 = 24h（affair.md §5.1：公示期吸收时钟偏差与副本滞后） */
export const MIN_PUB_PERIOD_HOURS = 24;

/**
 * 本插件声明的关闭条件（§5.2 op-count 形态）：投票操作（content + payload.kind
 * 'vote'）达 minQuorum 条即满足关闭门槛——内核逐副本确定性复算的正是这一
 * 计数（按 §8 opHash 升序取前 count 条为 countedOps）。
 */
export function buildVoteCloseCondition(minQuorum: number): Record<string, unknown> {
  return { type: 'op-count', opType: 'content', filter: 'vote', count: Math.max(1, Math.floor(minQuorum)) };
}

/**
 * 创世规则文档（affair.md §5，引擎 b1）：
 * - 客户端 draft 的公示期/参与门槛映射为协议字段；公示期协议下限 24h，
 *   低于下限在 validateAffairDraft 已被拒（此处防御性再钳制）；
 * - closeConditions 声明为内核可判定形态（§5.2）：单条 op-count 条件
 *   （opType=content + filter 'vote'，count = 法定人数）——「投票操作数达
 *   法定人数」是内核逐副本可复算的关闭门槛。内核 threshold 条件的基数语义
 *   不支持「按 payload.kind 过滤的赞成票比例」（基数内计入一切 content 操作，
 *   无 filter 维度），故 passThreshold 不作为内核关闭条件声明，由客户端计票
 *   把关（tally 明细随决议载荷入日志，任何副本可从同一操作集合重算核对）；
 * - passThreshold / minQuorum 是本插件的展示与客户端计票参数，内核不解释，
 *   收在 rules.sparkAffairs 下随规则文档一并被 affairId/rulesHash 承诺——
 *   「展示参数 = 计票参数 = 关闭条件参数」由同一份 draft 单源生成；
 * - 规则修改机制固定为 delayed-veto（§5.3 三形态之一；multisig m<2 被内核
 *   单点禁令拒绝，vote 机制需要快照名册，参考实现不引入）。
 */
export function buildRulesDoc(rules: AffairRules): Record<string, unknown> {
  const delayMs = Math.max(rules.reviewPeriodHours, MIN_PUB_PERIOD_HOURS) * 3600 * 1000;
  const doc: Record<string, unknown> = {
    engine: 'b1',
    closeConditions: [buildVoteCloseCondition(rules.minQuorum)],
    pubPeriod: { delayMs, vetoThreshold: { count: 1 } },
    ruleChange: { kind: 'delayed-veto', delayMs, vetoThreshold: { count: 1 } },
    exec: null,
    sparkAffairs: { passThreshold: rules.passThreshold, minQuorum: rules.minQuorum }
  };
  if (rules.entryRequirement.kind === 'ladder') {
    // 门槛作用于贡献层；表决层按 §六 默认本就要求投票者级
    doc.participation = {
      contribute: { ladder: rules.entryRequirement.minLevel },
      vote: { ladder: 'voter' },
      combine: 'all'
    };
  } else if (rules.entryRequirement.kind === 'credential') {
    // 凭证门槛需要验证人域声明（verifierDomain），本参考客户端不构造，
    // 由规则修改流程后续引入；创建期直接拒绝，不产出含糊的创世规则。
    throw new Error('本参考客户端暂不支持以凭证门槛发起议题（可通过集体决策的规则修改后续引入）');
  }
  return doc;
}

/** 内容操作载荷（opType=content 的插件语义部分；内核只验签名与门槛，不解释） */
export type AffairContentPayload =
  | ({ kind: 'contribution' } & ContributionPayload)
  | ({ kind: 'vote' } & VotePayload)
  | ({ kind: 'comment' } & CommentPayload);

/**
 * 操作条目草稿（剔除 sig；§3.2 线形）：
 * - opType 固定 content——插件语义载荷（贡献/投票/评论都在 payload.kind 上），
 *   内核枚举的 vote 只服务 rule-change/meta-revise（§4）；
 * - prevOpHash 是因果见证（首条 = affairId；之后取本地观察到的 DAG 头，
 *   多个头时取字典序最小者，仅为本地确定性选择，协议不做要求）；
 * - declaredAt 为签名者声明时刻（本机毫秒，自报值）；内核只用它做实时提交
 *   的新鲜度窗口判定（affair.md §3.1/§7.2：复制入站豁免，永不进入判定）。
 */
export function buildOpDraft(
  affairId: string,
  actor: AffairActor,
  kind: AffairOperationKind,
  payload: ContributionPayload | VotePayload | CommentPayload,
  heads: string[],
  declaredAt: number
): Record<string, unknown> {
  const knownHeads = [...heads].sort();
  return {
    opV: 1,
    affairId,
    opType: 'content',
    prevOpHash: knownHeads.length > 0 ? knownHeads[0] : affairId,
    payload: { kind, ...payload },
    actor: { kind: actor.kind, identity: actor.identity, publicKey: actor.publicKey },
    declaredAt
  };
}

/**
 * 决议操作草稿（opType=resolution，affair.md §6.1 线形）：
 * - payload 各字段由 model.planResolution 对「决议因果闭包」确定性算出：
 *   condition 逐字回引规则文档声明的关闭条件（内核复算 condition ∈ 规则）；
 *   countedOps = 闭包内 vote 操作按 §8 opHash 升序的前 minQuorum 条（内核
 *   复算逐字比对）；rulesHash / pubPeriod 与判定所用规则版本一致；
 * - result/tally 是插件语义（内核只承诺字节）：tally 记录名册过滤 +
 *   一人一票去重后的诚实计票，任何副本可从同一操作集合重算核对；
 * - prevOpHash = plan.prevOpHash（决议因果见证；countedOps 的有效性依赖
 *   决议只引用其祖先集合内的操作，planResolution 已按同一口径计算闭包）。
 */
export function buildResolutionDraft(
  affairId: string,
  actor: AffairActor,
  plan: ResolutionPlan,
  declaredAt: number
): Record<string, unknown> {
  return {
    opV: 1,
    affairId,
    opType: 'resolution',
    prevOpHash: plan.prevOpHash,
    payload: {
      result: plan.result,
      condition: plan.condition,
      countedOps: plan.countedOps,
      tally: plan.tally,
      rulesHash: plan.rulesHash,
      pubPeriod: { delayMs: plan.pubPeriodMs }
    },
    actor: { kind: actor.kind, identity: actor.identity, publicKey: actor.publicKey },
    declaredAt
  };
}
