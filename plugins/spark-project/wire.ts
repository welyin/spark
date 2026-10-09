/**
 * 项目（议题）插件（spark-project）· 协议线形构造（project.md §3 / affair.md §3.2）。
 *
 * 分层（与 spark-affairs/spark-kanban 同口径）：
 * - 通用协议线形（canonical/哈希/身份推导/创世草稿/refs 类型）上收
 *   packages/plugin-sdk/src/affair-wire.ts，经本文件 re-export；
 * - 创建事务走 sdk.affairs.create（SDK 承载创世构造 + 域身份签名 + follow
 *   全链校验）；本文件只承载插件语义层：项目/子事务创世输入组装（含规则
 *   文档模板与 publish 声明位）与内容操作载荷/条目草稿构造；
 * - 签名主体诚实口径（同 spark-affairs）：actor 为本插件域身份
 *   （kind:person 线形），不代表操作者个人身份。
 *
 * 本文件不依赖 SDK 运行时/Vue（affair-wire 同为纯函数模块），全部可单测。
 */

// 通用线形助手 re-export（唯一实现在 SDK affair-wire，与内核 canonical.rs 同向量）
export {
  base64Decode,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload
} from '../../packages/plugin-sdk/src/affair-wire';
export type { AffairActor, AffairGenesisInput, AffairRef } from '../../packages/plugin-sdk/src/affair-wire';

import type { AffairActor, AffairGenesisInput } from '../../packages/plugin-sdk/src/affair-wire';
import {
  CHILD_NOTICE_KIND,
  DISPOSITION_KIND,
  PROJECT_AFFAIR_TYPE,
  PROJECT_COMMENT_KIND,
  buildChildRulesDoc,
  buildProjectRulesDoc,
  normalizeProjectText,
  type ChildAffairType,
  type DispositionAction
} from './model';

// ------------------------------------------------------------------
// 创世输入组装（sdk.affairs.create 的类型化描述）
// ------------------------------------------------------------------

/**
 * 项目议题创世输入（§3.1）：
 * - type 'project'；title/summary/tags = 公共目录索引三要素；
 * - rules = 维护者制模板（maintainers 初始 = 发起人插件域身份 id，
 *   decisions 实施期补录口径）；
 * - publish 声明位（档二-2）：显式 true 时创世顶层携带 publish:true
 *   进全网公共目录——**调用方须先完成显式用户确认**（service 层
 *   fail-closed 复核 confirmedPublish，档二-2 补录约定）。
 */
export function buildProjectGenesisInput(
  input: { title: string; summary: string; tags: string[]; publish?: boolean },
  initiatorIdentity: string
): AffairGenesisInput {
  return {
    type: PROJECT_AFFAIR_TYPE,
    title: normalizeProjectText(input.title),
    summary: normalizeProjectText(input.summary),
    tags: input.tags.map((tag) => normalizeProjectText(tag)).filter(Boolean),
    rules: buildProjectRulesDoc([initiatorIdentity]),
    ...(input.publish === true ? { publish: true } : {})
  };
}

/**
 * 子事务创世输入（§3.1/§3.4）：独立事务，refs = [{target: 项目 affairId,
 * rel: 'parent'}]；rules 为维护者制快照（权威源仍是 parent 现行规则）；
 * PR 载荷（bundle/patch 的内容面 cid）经 extra.pr 显式声明（随 affairId
 * 被承诺，§3.1「插件语义顶层字段必须经 extra」）。
 */
export function buildChildGenesisInput(
  projectAffairId: string,
  input: { type: ChildAffairType; title: string; summary: string; bundleCid?: string },
  maintainersSnapshot: string[]
): AffairGenesisInput {
  return {
    type: input.type,
    title: normalizeProjectText(input.title),
    summary: normalizeProjectText(input.summary),
    refs: [{ target: projectAffairId, rel: 'parent' }],
    rules: buildChildRulesDoc(maintainersSnapshot),
    extra: {
      sparkProject: { childAffair: true },
      ...(input.type === 'pr' && input.bundleCid?.trim()
        ? { pr: { bundleCid: input.bundleCid.trim() } }
        : {})
    }
  };
}

// ------------------------------------------------------------------
// 内容操作（opType=content；payload.kind 承载插件语义，内核不解释）
// ------------------------------------------------------------------

export type ProjectOpPayload =
  | { kind: typeof PROJECT_COMMENT_KIND; text: string }
  | { kind: typeof CHILD_NOTICE_KIND; childAffairId: string; childType: string; title: string }
  | { kind: typeof DISPOSITION_KIND; action: DispositionAction; note?: string };

export function buildCommentPayload(text: string): ProjectOpPayload {
  return { kind: PROJECT_COMMENT_KIND, text: normalizeProjectText(text) };
}

export function buildChildNoticePayload(child: {
  childAffairId: string;
  childType: string;
  title: string;
}): ProjectOpPayload {
  return { kind: CHILD_NOTICE_KIND, ...child };
}

export function buildDispositionPayload(action: DispositionAction, note?: string): ProjectOpPayload {
  return {
    kind: DISPOSITION_KIND,
    action,
    ...(note?.trim() ? { note: normalizeProjectText(note) } : {})
  };
}

/**
 * 操作条目草稿（剔除 sig；§3.2 线形，与 spark-kanban/wire.ts 同构）：
 * - prevOpHash 是因果见证（首条 = affairId；多个 DAG 头取字典序最小者，
 *   仅为本地确定性选择，协议不做要求）；
 * - declaredAt 为签名者声明时刻（本机毫秒，自报值）；内核只用于实时提交
 *   新鲜度窗口判定，永不进入判定与排序（事务协议 §7.2-4 红线）。
 */
export function buildOpDraft(
  affairId: string,
  actor: AffairActor,
  payload: ProjectOpPayload,
  heads: string[],
  declaredAt: number
): Record<string, unknown> {
  const knownHeads = [...heads].sort();
  return {
    opV: 1,
    affairId,
    opType: 'content',
    prevOpHash: knownHeads.length > 0 ? knownHeads[0] : affairId,
    payload,
    actor: { kind: actor.kind, identity: actor.identity, publicKey: actor.publicKey },
    declaredAt
  };
}

// ------------------------------------------------------------------
// 处置决议操作（opType=resolution；R1：bug/proposal 处置走内核公示期，
// 线形对齐 spark-affairs/wire.ts buildResolutionDraft 先例，affair.md §6.1）
// ------------------------------------------------------------------

/** 处置决议计划（各字段内核逐副本复算，编造即被 replay 拒绝） */
export type DispositionResolutionPlan = {
  /** 结果标签 = 处置动作（内核要求 ≤32 字符串；本插件 = adopted/closed） */
  result: DispositionAction;
  /** 逐字回引规则文档 closeConditions 项（内核复算 condition ∈ 规则） */
  condition: Record<string, unknown>;
  /** 计入关闭判定的操作 opHash（§8 升序；本插件 = [处置动议 opHash]） */
  countedOps: string[];
  /** 判定所用规则文档版本哈希（sdk.affairs.readRules 的 current.rulesHash） */
  rulesHash: string;
  /** 公示期毫秒（须与判定所用 rules 版本 pubPeriod.delayMs 逐字一致，≥24h） */
  pubPeriodMs: number;
};

/**
 * 处置决议操作草稿（剔除 sig）：
 * - prevOpHash 必须使 countedOps 落在决议的祖先闭包内——调用方传处置动议
 *   的 opHash（动议入日志后它即本地 DAG 头），内核复算 countedOps 有效性
 *   依赖此因果见证；
 * - tally 缺省不携带（内核缺省 Null；单维护者动议无计票明细可言，诚实为空）。
 */
export function buildResolutionDraft(
  affairId: string,
  actor: AffairActor,
  plan: DispositionResolutionPlan,
  prevOpHash: string,
  declaredAt: number
): Record<string, unknown> {
  return {
    opV: 1,
    affairId,
    opType: 'resolution',
    prevOpHash,
    payload: {
      result: plan.result,
      condition: plan.condition,
      countedOps: [...plan.countedOps].sort(),
      rulesHash: plan.rulesHash,
      pubPeriod: { delayMs: plan.pubPeriodMs }
    },
    actor: { kind: actor.kind, identity: actor.identity, publicKey: actor.publicKey },
    declaredAt
  };
}
