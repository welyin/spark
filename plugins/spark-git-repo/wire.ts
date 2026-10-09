/**
 * 代码仓库应用（spark-git-repo）· 协议线形构造（git-repo.md §3.2 / affair.md §3.2）。
 *
 * 分层（与 spark-affairs/wire.ts 同口径）：
 * - 通用协议线形（canonical/哈希/身份推导）上收 packages/plugin-sdk/src/affair-wire.ts，
 *   经本文件 re-export；事务创建走 sdk.affairs.create（SDK 承载创世构造 + 签名 +
 *   follow），refs 走 §10 类型化枚举（PR 子事务 → 项目议题为 parent 引用）；
 * - 本文件只承载插件语义层：PR 操作集（pr.open/update/comment/review/merged/closed）
 *   与镜像清单操作（git.mirror.manifest，项目议题日志内）的载荷构造与操作条目草稿；
 * - 操作签名主体仍是本插件域身份（SDK 只有域身份签名面）——协议 actor 为插件域
 *   身份（kind:person 线形），不代表操作者个人身份（诚实口径，同 spark-affairs）。
 *
 * 本文件不依赖 SDK 运行时/Vue（affair-wire 同为纯函数模块），全部可单测。
 */

import type {
  MirrorManifest,
  PrAttachment,
  PrClosedPayload,
  PrCommentPayload,
  PrMergedPayload,
  PrOpenPayload,
  PrOpKind,
  PrReviewPayload,
  PrUpdatePayload
} from './model';
import { MIRROR_MANIFEST_KIND } from './model';

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

/** PR 子事务 → 项目议题的 parent 引用（affair.md §10 四种引用之父子） */
export function buildParentRef(projectAffairId: string): { target: string; rel: 'parent' } {
  return { target: projectAffairId, rel: 'parent' };
}

// ------------------------------------------------------------------
// PR 操作载荷（git-repo.md §3.2 表）
// ------------------------------------------------------------------

export function buildPrOpenPayload(input: PrOpenPayload): Record<string, unknown> {
  return {
    kind: 'pr.open' satisfies PrOpKind,
    title: input.title.trim(),
    description: input.description,
    base: input.base,
    head: input.head,
    attachments: input.attachments.map(attachmentWire)
  };
}

export function buildPrUpdatePayload(input: PrUpdatePayload): Record<string, unknown> {
  return {
    kind: 'pr.update' satisfies PrOpKind,
    head: input.head,
    attachments: input.attachments.map(attachmentWire),
    ...(input.note !== undefined ? { note: input.note } : {})
  };
}

export function buildPrCommentPayload(input: PrCommentPayload): Record<string, unknown> {
  return {
    kind: 'pr.comment' satisfies PrOpKind,
    text: input.text,
    ...(input.ref !== undefined ? { ref: { path: input.ref.path, line: input.ref.line } } : {})
  };
}

export function buildPrReviewPayload(input: PrReviewPayload): Record<string, unknown> {
  return {
    kind: 'pr.review' satisfies PrOpKind,
    verdict: input.verdict,
    text: input.text
  };
}

/** 合并回执（档一-3：单维护者回执即生效；resultCommit + 新镜像版本指针） */
export function buildPrMergedPayload(input: PrMergedPayload): Record<string, unknown> {
  return {
    kind: 'pr.merged' satisfies PrOpKind,
    resultCommit: input.resultCommit,
    mirrorVersion: input.mirrorVersion,
    ...(input.note !== undefined ? { note: input.note } : {})
  };
}

export function buildPrClosedPayload(input: PrClosedPayload): Record<string, unknown> {
  return {
    kind: 'pr.closed' satisfies PrOpKind,
    reason: input.reason
  };
}

function attachmentWire(attachment: PrAttachment): Record<string, unknown> {
  return {
    kind: attachment.kind,
    cid: attachment.cid,
    size: attachment.size,
    ...(attachment.name !== undefined ? { name: attachment.name } : {})
  };
}

// ------------------------------------------------------------------
// 镜像清单操作载荷（档二-5：项目议题内签名事务操作，清单即账本信息）
// ------------------------------------------------------------------

export function buildMirrorManifestPayload(manifest: MirrorManifest): Record<string, unknown> {
  return {
    kind: MIRROR_MANIFEST_KIND,
    repo: manifest.repo,
    defaultBranch: manifest.defaultBranch,
    branches: manifest.branches.map((branch) => ({ name: branch.name, head: branch.head })),
    version: manifest.version,
    objects: manifest.objects.map((ref) => ({ sha: ref.sha, type: ref.type, cid: ref.cid })),
    ...(manifest.importHead !== undefined ? { importHead: manifest.importHead } : {}),
    ...(manifest.note !== undefined ? { note: manifest.note } : {})
  };
}

// ------------------------------------------------------------------
// 操作条目草稿（剔除 sig；affair.md §3.2 线形，与 spark-affairs 同构）
// ------------------------------------------------------------------

/**
 * 内容操作条目草稿（opType=content——插件语义载荷在 payload.kind 上，内核不解释）：
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
