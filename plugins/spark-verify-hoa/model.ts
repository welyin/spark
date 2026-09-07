/**
 * 业主资格验证示例（spark-verify-hoa）· 数据模型与纯函数。
 *
 * 语义对齐 wiki/product/community-model.md §十（身份验证：验证即插件）：
 * - 验证插件 = 方法（本插件），被组织信任的是有权签发凭证的**验证人身份**；
 * - 产物是资格凭证：「身份 X 是小区 Y 的某户业主/居住者，核验方式 Z」；
 * - 证据最小披露：原始材料只给验证人看、不进公共数据不上链；凭证只暴露
 *   资格结论（户号），不暴露姓名与证件号——本文件以敏感字段禁令强制之；
 * - 凭证不设有效期；资格变更由验证人主动发起注销（注销不抹除历史）。
 *
 * 本文件不依赖 SDK / Vue，全部可单测。
 */

/** 凭证类型：业主凭证 / 居住凭证（租户等非业主居住者，§十） */
export type HoaCredentialType = 'owner' | 'resident';

/** 验证方法（组织在加入规则中声明接受哪些组合，§十——此处为业主场景示例） */
export type VerificationMethod = 'deed-manual' | 'vouch-2' | 'gov-realname';

/** 材料摘要（进同步集合的公开形态：只有标签 + 内容哈希 + 备注，不含原文） */
export type MaterialSummary = {
  label: string;
  contentHash: string;
  byteSize: number;
  note?: string;
};

/** 申请材料原文（本地留存、显式 __sync:false，只给验证人看） */
export type MaterialDraft = {
  label: string;
  content: string;
};

export type VerificationApplication = {
  applicationId: string;
  orgId: string;
  applicantRootId: string;
  method: VerificationMethod;
  credentialType: HoaCredentialType;
  /** 户号（如 3-502）：凭证也只暴露到这个粒度 */
  unitNo: string;
  materials: MaterialSummary[];
  createdAt: number;
};

/**
 * 资格凭证（演示级线形，append-only，随组织同步分发）。
 *
 * 诚实口径（评审阻塞 3/4，README 同步声明）：
 * - 线形与协议凭证（credential §2：credV/credType/issuer/holder/subjectDomain/
 *   claims/method/linkRef/issuedAt/sig，credId = canonical 复算）不同——
 *   不进 cred:held: 键域、过不了内核验证链（结构→credId 复算→验签→信任匹配→
 *   注销检查），与内核凭证体系零互操作；只在本插件集合内演示「验证即插件」
 *   的流程分工，不得呈现为可被事务客户端/内核消费的资格凭证；
 * - 签名主体是插件域身份（sdk.identity.sign 只有域签名面），密码学上不证明
 *   验证人个人身份；issuerRootId 为自报文本。
 */
export type HoaCredential = {
  credentialId: string;
  /** 签发所依据的申请 */
  applicationId: string;
  orgId: string;
  /** 被签发人（个人 rootId） */
  subjectRootId: string;
  credentialType: HoaCredentialType;
  unitNo: string;
  method: VerificationMethod;
  issuerRootId: string;
  issuedAt: number;
  signature: {
    payload: string;
    signature: string;
    publicKey: string;
  };
};

/** 注销记录（演示级，验证人主动发起；既往不咎——撤销信任不影响已签发凭证效力，§十） */
export type CredentialRevocation = {
  revocationId: string;
  credentialId: string;
  reason: string;
  revokedBy: string;
  revokedAt: number;
  signature: {
    payload: string;
    signature: string;
    publicKey: string;
  };
};

/** 敏感字段禁令（证据最小披露）：任何进同步集合的记录不得含这些键 */
export const SENSITIVE_FIELD_KEYS = ['name', 'realName', 'idCardNo', 'idNumber', 'phone', 'phoneNumber'] as const;

export const UNIT_NO_MAX_LENGTH = 20;
export const MATERIAL_LABEL_MAX_LENGTH = 40;
export const MATERIAL_CONTENT_MAX_LENGTH = 4096;
export const REVOCATION_REASON_MAX_LENGTH = 200;

/** 材料内容哈希（FNV-1a 32bit，hex；与 spark-affairs 同款取舍，见该插件 model） */
export function hashMaterialContent(content: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i += 1) {
    hash ^= content.charCodeAt(i);
    hash = (hash + ((hash << 1) + (hash << 4) + (hash << 7) + (hash << 8) + (hash << 24))) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/** 申请输入校验（申请人侧引导的第一步） */
export function validateApplicationInput(input: {
  method: VerificationMethod;
  unitNo: string;
  credentialType: HoaCredentialType;
  materials: MaterialDraft[];
}): { ok: boolean; reason?: string } {
  if (!input.unitNo.trim()) {
    return { ok: false, reason: '户号不能为空' };
  }
  if (input.unitNo.trim().length > UNIT_NO_MAX_LENGTH) {
    return { ok: false, reason: `户号不能超过${UNIT_NO_MAX_LENGTH}字符` };
  }
  if (input.materials.length === 0) {
    return { ok: false, reason: '至少提交一份材料' };
  }
  for (const material of input.materials) {
    if (!material.label.trim() || !material.content.trim()) {
      return { ok: false, reason: '材料标签与内容不能为空' };
    }
    if (material.label.length > MATERIAL_LABEL_MAX_LENGTH) {
      return { ok: false, reason: `材料标签不能超过${MATERIAL_LABEL_MAX_LENGTH}字符` };
    }
    if (material.content.length > MATERIAL_CONTENT_MAX_LENGTH) {
      return { ok: false, reason: `单份材料内容不能超过${MATERIAL_CONTENT_MAX_LENGTH}字符` };
    }
  }
  return { ok: true };
}

/**
 * 敏感字段扫描：递归检查记录键名，命中 SENSITIVE_FIELD_KEYS 即报错。
 * 在写同步集合前调用（service 层强制）——证据最小披露是产品硬规则，
 * 不靠申请人自觉。
 */
export function assertNoSensitiveFields(record: unknown, path = ''): void {
  if (Array.isArray(record)) {
    record.forEach((item, index) => assertNoSensitiveFields(item, `${path}[${index}]`));
    return;
  }
  if (typeof record !== 'object' || record === null) {
    return;
  }
  for (const [key, value] of Object.entries(record as Record<string, unknown>)) {
    const fullKey = path ? `${path}.${key}` : key;
    if ((SENSITIVE_FIELD_KEYS as readonly string[]).includes(key)) {
      throw new Error(`记录含敏感字段 ${fullKey}（证据最小披露：姓名/证件号/电话不得进入同步数据）`);
    }
    assertNoSensitiveFields(value, fullKey);
  }
}

/**
 * 凭证签名载荷：`credential:{orgId}:{credentialId}:{subject}:{type}:{unitNo}:{method}`。
 * 绑定小区、被签发人、凭证类型、户号与核验方式——验签侧重算比对，防搬用替换。
 */
export function buildCredentialSignPayload(
  orgId: string,
  credentialId: string,
  subjectRootId: string,
  credentialType: HoaCredentialType,
  unitNo: string,
  method: VerificationMethod
): string {
  return `credential:${orgId}:${credentialId}:${subjectRootId}:${credentialType}:${unitNo}:${method}`;
}

/** 注销签名载荷（绑定凭证与理由） */
export function buildRevocationSignPayload(credentialId: string, reason: string): string {
  return `revocation:${credentialId}:${hashMaterialContent(reason)}`;
}

/** 申请状态推导：append-only 集合不允许覆盖状态，由凭证/注销记录推导（诚实口径） */
export type ApplicationStatus = 'pending' | 'issued' | 'revoked';

export function deriveApplicationStatus(
  application: VerificationApplication,
  credentials: HoaCredential[],
  revocations: CredentialRevocation[]
): ApplicationStatus {
  const credential = credentials.find((item) => item.applicationId === application.applicationId);
  if (!credential) {
    return 'pending';
  }
  return revocations.some((item) => item.credentialId === credential.credentialId) ? 'revoked' : 'issued';
}
