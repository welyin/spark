/**
 * 共同体事务协议线形（wiki/protocol/community/affair.md §2；SDK 共享实现）。
 *
 * sdk.affairs.create 的创世记录构造层：创建语义 = 本地构造创世记录 →
 * 插件域身份签名 → affairs.follow（内核全链校验 + affairId 自认证复算）。
 * 创世要过内核全链校验（结构 → 自认证复算 → 验签 → 规则静态检查），故
 * 本模块逐字节对齐协议线形：
 * - canonical 签名载荷 = normalizeObject(记录剔除 sig)（sync-evidence §1 的
 *   JS 口径，内核 core/src/evidence/canonical.rs 逐字节复刻同一规则）；
 * - 身份 id = sha256hex(base64decode(publicKey))（§2.2 自包含绑定）；
 * - 签名主体是调用方插件域身份（SDK 只有域身份签名面）——initiator/actor
 *   为插件域身份（kind: person 线形），不代表发起人个人身份。
 *
 * 本模块不依赖桥/Vue，全部可单测（验收断言与内核 canonical.rs 同向量）。
 */

// ------------------------------------------------------------------
// 事务间引用（affair.md §10；内核 core/src/affair/refs.rs 同枚举）
// ------------------------------------------------------------------

/** 引用关系枚举（§10 表；内核 parse_ref_rel 逐字对齐） */
export type AffairRefRel = 'inherit' | 'appeal' | 'parent' | 'related';

/** 引用关系枚举全集（客户端下拉/校验用） */
export const AFFAIR_REF_RELS: readonly AffairRefRel[] = ['inherit', 'appeal', 'parent', 'related'];

/**
 * 事务间引用条目（§10）：目标事务 ID + 关系类型。append-only 不可撤销；
 * 自指禁令（target == 本事务 affairId）由内核在 affairId 复算后 enforced，
 * 客户端只做形状校验。
 */
export type AffairRef = {
  /** 目标事务 affairId（64 hex） */
  target: string;
  rel: AffairRefRel;
};

/** 引用条目形状校验（rel ∈ §10 枚举、target 为 64 位小写 hex；自指禁令归内核） */
export function validateAffairRefs(refs: AffairRef[]): { ok: boolean; reason?: string } {
  for (const ref of refs) {
    if (!AFFAIR_REF_RELS.includes(ref.rel)) {
      return { ok: false, reason: `未知引用关系 ${String(ref.rel)}（§10 枚举：${AFFAIR_REF_RELS.join('/')}）` };
    }
    if (!/^[0-9a-f]{64}$/.test(ref.target)) {
      return { ok: false, reason: '引用目标必须是 64 位小写 hex 的事务 affairId' };
    }
  }
  return { ok: true };
}

// ------------------------------------------------------------------
// canonical JSON（normalizeObject）与哈希/编码
// ------------------------------------------------------------------

/**
 * canonical JSON（sync-evidence §1 的 JS normalizeObject 原样语义）：
 * 非 object 直接 JSON.stringify；object/数组 key 排序后，每个值先递归
 * normalize 再作为字符串值嵌入（嵌套对象被序列化成 JSON 字符串）。
 * JS 对象整数型 key 恒按数值升序前置，与内核 canonical.rs 逐字节一致。
 */
export function normalizeObject(value: unknown): string {
  if (value === undefined) {
    return 'undefined';
  }
  if (value === null || typeof value !== 'object') {
    return JSON.stringify(value) as string;
  }
  const record = value as Record<string, unknown>;
  const wrapped: Record<string, string> = {};
  for (const key of Object.keys(record).sort()) {
    wrapped[key] = normalizeObject(record[key]);
  }
  return JSON.stringify(wrapped);
}

/** base64 解码（不依赖 atob：插件运行环境不做假设） */
export function base64Decode(text: string): Uint8Array {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const clean = text.replace(/=+$/, '');
  const bytes: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of clean) {
    const value = alphabet.indexOf(ch);
    if (value < 0) {
      throw new Error('invalid base64');
    }
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      bytes.push((buffer >> bits) & 0xff);
    }
  }
  return Uint8Array.from(bytes);
}

const SHA256_K = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
];

/**
 * SHA-256（纯 TS 实现）：协议的身份 id / affairId / opHash 全部是 sha256hex，
 * 而插件沙箱为 opaque origin，不假设 WebCrypto 可用。
 */
export function sha256HexBytes(input: Uint8Array): string {
  const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));
  const h = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const byteLength = input.length;
  const bitLength = byteLength * 8;
  const paddedLength = (((byteLength + 8) >> 6) + 1) << 6;
  const padded = new Uint8Array(paddedLength);
  padded.set(input);
  padded[byteLength] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(paddedLength - 4, bitLength >>> 0);
  view.setUint32(paddedLength - 8, Math.floor(bitLength / 0x100000000));
  const w = new Array<number>(64);
  for (let block = 0; block < padded.length; block += 64) {
    for (let t = 0; t < 16; t += 1) {
      w[t] = view.getUint32(block + t * 4);
    }
    for (let t = 16; t < 64; t += 1) {
      const s0 = rotr(w[t - 15], 7) ^ (rotr(w[t - 15], 18)) ^ (w[t - 15] >>> 3);
      const s1 = rotr(w[t - 2], 17) ^ (rotr(w[t - 2], 19)) ^ (w[t - 2] >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = h;
    for (let t = 0; t < 64; t += 1) {
      const s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const temp1 = (hh + s1 + ch + SHA256_K[t] + w[t]) >>> 0;
      const s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const temp2 = (s0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + temp1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) >>> 0;
    }
    h[0] = (h[0] + a) >>> 0;
    h[1] = (h[1] + b) >>> 0;
    h[2] = (h[2] + c) >>> 0;
    h[3] = (h[3] + d) >>> 0;
    h[4] = (h[4] + e) >>> 0;
    h[5] = (h[5] + f) >>> 0;
    h[6] = (h[6] + g) >>> 0;
    h[7] = (h[7] + hh) >>> 0;
  }
  return h.map((word) => word.toString(16).padStart(8, '0')).join('');
}

/** sha256hex(UTF-8 文本) */
export function sha256Hex(text: string): string {
  return sha256HexBytes(new TextEncoder().encode(text));
}

/** 身份 id 自包含推导（§2.2）：sha256hex(base64decode(publicKey)) */
export function deriveIdentity(publicKeyBase64: string): string {
  return sha256HexBytes(base64Decode(publicKeyBase64));
}

// ------------------------------------------------------------------
// 创世记录构造（剔除 sig 的草稿；签名载荷 = canonical 全文）
// ------------------------------------------------------------------

/** 协议 actor（§2.2）：identity == sha256hex(base64decode(publicKey)) 自包含绑定 */
export type AffairActor = {
  kind: 'person';
  identity: string;
  publicKey: string;
};

/**
 * 创世记录输入（sdk.affairs.create 的类型化描述）：
 * 字段集合即 canonical/affairId 承诺的全集——内核不解释未知字段但会承诺之，
 * 故插件语义顶层字段必须经 `extra` 显式声明（如 regionCode/initialVoters）。
 */
export type AffairGenesisInput = {
  /** 事务类型标识（插件命名空间，内核不解释；§2.1 形状） */
  type: string;
  title: string;
  summary: string;
  tags?: string[];
  /** 事务间引用（§10；缺省 = 空数组） */
  refs?: AffairRef[];
  /** 创世规则文档（§5；引擎/公示期/规则修改机制由内核静态检查） */
  rules: Record<string, unknown>;
  /** 插件语义顶层字段（原样并入创世记录，随 affairId 被承诺） */
  extra?: Record<string, unknown>;
};

/**
 * 创世记录草稿（剔除 sig；§2.1 字段约束）：
 * - initiator = 调用方插件域身份 actor（签名主体诚实口径，见模块头注）；
 * - refs 原样携带（形状校验先行；自指禁令由内核在 affairId 复算后 enforced）；
 * - createdAt 为创建方声明时刻（协议字段，权威时间以存证链为准）。
 */
export function buildGenesisDraft(
  input: AffairGenesisInput,
  actor: AffairActor,
  createdAt: number
): Record<string, unknown> {
  const refs = input.refs ?? [];
  const verdict = validateAffairRefs(refs);
  if (!verdict.ok) {
    throw new Error(verdict.reason);
  }
  return {
    affairV: 1,
    type: input.type,
    title: input.title.trim(),
    summary: input.summary.trim(),
    tags: (input.tags ?? []).map((tag) => tag.trim()).filter(Boolean),
    initiator: { kind: actor.kind, identity: actor.identity, publicKey: actor.publicKey },
    refs: refs.map((ref) => ({ target: ref.target, rel: ref.rel })),
    createdAt,
    rules: input.rules,
    ...(input.extra ?? {})
  };
}

/** 签名载荷 = canonical(记录剔除 sig)（community README 总约） */
export function signPayload(record: Record<string, unknown>): string {
  const sans: Record<string, unknown> = { ...record };
  delete sans.sig;
  return normalizeObject(sans);
}
