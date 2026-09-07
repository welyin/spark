/**
 * 担保链门槛示例（spark-threshold-vouch）· 数据模型与纯函数。
 *
 * 定位（wiki/architecture/community-affairs.md §7.3）：门槛插件（担保链/成本类）
 * 产出「是否满足门槛」的凭证或签名证明，**内核只验证产物**——担保流程的
 * 业务语义（谁可以担保、担保算什么）在插件，协议无地位。
 *
 * 流程：被担保人发起请求（N 名已有参与者担保）→ 各担保经 identity:sign
 * 签名 → 收齐 N 份后组装 ThresholdProof（组装时再签一次，防组装后被
 * 篡改）→ 事务客户端免权限验签产物。
 *
 * 诚实口径（评审阻塞 3）：所有签名的主体是插件域身份，不证明担保人/组装人
 * 个人身份；Vouch.voucherRootId、ThresholdProof.assembledBy 均为自报文本。
 * 本文件不依赖 SDK / Vue，全部可单测。
 */

/** 担保请求 */
export type VouchRequest = {
  requestId: string;
  /** 治理上下文（事务 id / 项目空间）：门槛证明只在声明的上下文内有效 */
  context: string;
  subjectRootId: string;
  /** 门槛：需要的不同担保人数量 N */
  requiredCount: number;
  note: string;
  createdAt: number;
};

/** 单份担保（插件域身份签名；voucherRootId 为自报文本，不证明担保人个人身份） */
export type Vouch = {
  requestId: string;
  voucherRootId: string;
  /** 被签名的载荷（buildVouchPayload 产物；验证侧重算比对） */
  payload: string;
  signature: string;
  publicKey: string;
  vouchedAt: number;
};

/** 门槛证明：「是否满足门槛」的签名产物，内核/客户端只验证它（演示级：签名主体为插件域身份） */
export type ThresholdProof = {
  proofId: string;
  requestId: string;
  context: string;
  subjectRootId: string;
  requiredCount: number;
  vouches: Vouch[];
  assembledBy: string;
  assembledAt: number;
  /** 组装签名对整个证明的载荷绑定（防组装后被偷换担保内容）；签名主体为插件域身份，assembledBy 为自报文本 */
  payload: string;
  signature: string;
  publicKey: string;
};

export const REQUEST_NOTE_MAX_LENGTH = 200;
export const CONTEXT_MAX_LENGTH = 120;
export const MAX_REQUIRED_COUNT = 100;

/** 确定性哈希（FNV-1a 32bit，hex；同款取舍见 spark-affairs model） */
export function hashText(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

export function validateRequestInput(input: {
  context: string;
  subjectRootId: string;
  requiredCount: number;
  note: string;
}): { ok: boolean; reason?: string } {
  if (!input.context.trim()) {
    return { ok: false, reason: '治理上下文不能为空（如事务 id）' };
  }
  if (input.context.trim().length > CONTEXT_MAX_LENGTH) {
    return { ok: false, reason: `上下文不能超过${CONTEXT_MAX_LENGTH}字符` };
  }
  if (!input.subjectRootId.trim()) {
    return { ok: false, reason: '被担保人不能为空' };
  }
  if (!Number.isInteger(input.requiredCount) || input.requiredCount < 1 || input.requiredCount > MAX_REQUIRED_COUNT) {
    return { ok: false, reason: `门槛人数须为 1-${MAX_REQUIRED_COUNT} 的整数` };
  }
  if (input.note.trim().length > REQUEST_NOTE_MAX_LENGTH) {
    return { ok: false, reason: `说明不能超过${REQUEST_NOTE_MAX_LENGTH}字` };
  }
  return { ok: true };
}

/**
 * 担保签名载荷：`vouch:{requestId}:{context}:{subject}:{voucher}`。
 * 绑定请求、上下文、被担保人与担保人——担保无法剪贴到别的请求/别人头上重放。
 */
export function buildVouchPayload(
  requestId: string,
  context: string,
  subjectRootId: string,
  voucherRootId: string
): string {
  return `vouch:${requestId}:${context}:${subjectRootId}:${voucherRootId}`;
}

/** 不同担保人计数（同一担保人重复担保只计一次——一人一票不加权） */
export function countDistinctVouchers(vouches: Vouch[]): number {
  return new Set(vouches.map((vouch) => vouch.voucherRootId)).size;
}

/** 门槛判定：不同担保人数量 >= N；被担保人自己担保不计（自查不构成担保） */
export function isThresholdMet(vouches: Vouch[], requiredCount: number, subjectRootId: string): boolean {
  const valid = vouches.filter((vouch) => vouch.voucherRootId !== subjectRootId);
  return countDistinctVouchers(valid) >= requiredCount;
}

/** 证明签名载荷：绑定证明标识、请求、上下文、被担保人与担保集合哈希 */
export function buildProofPayload(
  proofId: string,
  requestId: string,
  context: string,
  subjectRootId: string,
  vouches: Vouch[]
): string {
  const vouchSetHash = hashText(
    vouches
      .map((vouch) => vouch.payload)
      .sort()
      .join('|')
  );
  return `proof:${proofId}:${requestId}:${context}:${subjectRootId}:${vouchSetHash}`;
}

/**
 * 证明结构自检（组装前调用）：门槛必须已满足、担保集合内不得有重复担保人
 * （重复只计一次，放进证明会误导验签方的计数直觉）。
 */
export function assertAssemblable(request: VouchRequest, vouches: Vouch[]): void {
  if (!isThresholdMet(vouches, request.requiredCount, request.subjectRootId)) {
    throw new Error(
      `担保不足：${countDistinctVouchers(vouches.filter((v) => v.voucherRootId !== request.subjectRootId))}/${request.requiredCount}`
    );
  }
  const seen = new Set<string>();
  for (const vouch of vouches) {
    if (seen.has(vouch.voucherRootId)) {
      throw new Error(`担保人 ${vouch.voucherRootId} 重复担保，请先选择去重`);
    }
    seen.add(vouch.voucherRootId);
  }
}
