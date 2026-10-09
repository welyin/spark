/**
 * 任务看板插件（spark-kanban）· 协议线形构造（kanban.md §3.4 / affair.md §3.2）。
 *
 * 分层（与 spark-git-repo/wire.ts 同口径）：
 * - 通用协议线形（canonical/哈希/身份推导）上收 packages/plugin-sdk/src/affair-wire.ts，
 *   经本文件 re-export；
 * - 本文件只承载插件语义层：子事务状态操作（kanban.status，档三-1 拍板——中间态
 *   唯一权威源 = 子事务内签名状态操作）的载荷与操作条目草稿构造；
 * - 签名主体诚实口径（同 spark-affairs/spark-git-repo）：actor 为本插件域身份
 *   （kind:person 线形），不代表操作者个人身份；看板拖动转列产生的事务操作
 *   署名主体因此是插件域身份，UI 不声称「某成员个人」。
 *
 * 本文件不依赖 SDK 运行时/Vue（affair-wire 同为纯函数模块），全部可单测。
 */

import { KANBAN_STATUS_OP_KIND } from './model';

// 通用线形助手 re-export（唯一实现在 SDK affair-wire，与内核 canonical.rs 同向量）
export {
  base64Decode,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload
} from '../../packages/plugin-sdk/src/affair-wire';
export type { AffairActor, AffairRef } from '../../packages/plugin-sdk/src/affair-wire';

/**
 * 状态操作载荷（opType=content 的插件语义部分，内核不解释）：
 * - kind 固定 kanban.status；status 为目标列在看板配置中声明的 statusKey
 *   （档三-21：列—状态映射放看板配置插件集合）；
 * - prevStatus 捎带提交时本副本观察到的前一状态（溯源/审计友好，推导不依赖它——
 *   当前状态按确定性排序键取最新有效状态操作，见 model.deriveStatusKeyFromLog）；
 * - note 为可选附言（拖动转列时用户可留一句话，进事务日志可考）。
 */
export function buildStatusOpPayload(input: {
  status: string;
  prevStatus?: string | null;
  note?: string;
}): Record<string, unknown> {
  return {
    kind: KANBAN_STATUS_OP_KIND,
    status: input.status.trim(),
    ...(input.prevStatus ? { prevStatus: input.prevStatus } : {}),
    ...(input.note?.trim() ? { note: input.note.trim() } : {})
  };
}

/**
 * 内容操作条目草稿（剔除 sig；affair.md §3.2 线形，与 spark-affairs/spark-git-repo
 * 同构）：
 * - opType 固定 content——状态操作是插件语义载荷，内核只验签名与门槛；
 * - prevOpHash 是因果见证（首条 = affairId；之后取本地观察到的 DAG 头，多个头
 *   时取字典序最小者，仅为本地确定性选择，协议不做要求）；
 * - declaredAt 为签名者声明时刻（本机毫秒，自报值；内核只用于实时提交新鲜度
 *   窗口判定，复制入站豁免，永不进入判定）。
 */
export function buildOpDraft(
  affairId: string,
  actor: { kind: string; identity: string; publicKey: string },
  payload: Record<string, unknown>,
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
