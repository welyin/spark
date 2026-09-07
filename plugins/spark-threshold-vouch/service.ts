/**
 * 担保链门槛示例（spark-threshold-vouch）· 业务服务层。
 *
 * 职责边界（community-affairs.md §7.3）：门槛插件产出「是否满足门槛」的
 * 签名证明，内核/事务客户端只验证产物。本服务因此分两半：
 * - 流程侧（插件业务语义）：发起请求、收集担保、组装证明——docs 存储 +
 *   identity:sign 签名；
 * - 产物侧（免权限可验）：verifyProof 重算全部签名载荷 + identity.verify
 *   验签，任何节点对同一证明得到同一结论。
 *
 * 诚实口径（评审阻塞 3 的降级处理，README 同步声明）：identity:sign 只有
 * 插件域签名面——所有担保与组装签名的主体都是本插件域身份
 * （plugin:spark-threshold-vouch 派生钥匙），密码学上不证明担保人/组装人
 * 个人身份；voucherRootId / assembledBy 为自报文本，恶意用户可为任意
 * 「担保人」伪造担保且验签通过——「免权限可验」验的只是插件域钥匙与载荷
 * 完整性，不是担保人本人。平台层提供个人身份签名路径后应迁移。
 */
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  assertAssemblable,
  buildProofPayload,
  buildVouchPayload,
  countDistinctVouchers,
  isThresholdMet,
  validateRequestInput,
  type ThresholdProof,
  type Vouch,
  type VouchRequest
} from './model';

export const VOUCH_COLLECTIONS = {
  requests: 'vouch_requests',
  vouches: 'vouches',
  proofs: 'vouch_proofs'
} as const;

const COLLECTION_SCHEMAS = {
  [VOUCH_COLLECTIONS.requests]: { syncStrategy: 'append-only' as const },
  [VOUCH_COLLECTIONS.vouches]: { syncStrategy: 'append-only' as const },
  [VOUCH_COLLECTIONS.proofs]: { syncStrategy: 'append-only' as const }
};

/** 演示用途 id（Date.now + 随机）：演示数据可接受；正式数据 id 应内容寻址或单调序号 */
function newId(prefix: string): string {
  return `${prefix}_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

export class VouchService {
  private collectionsReady: Promise<void> | null = null;

  constructor(private readonly sdk: PluginSDK) {}

  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  /** 发起担保请求（被担保人侧） */
  async createRequest(input: {
    context: string;
    subjectRootId: string;
    requiredCount: number;
    note: string;
  }): Promise<VouchRequest> {
    const verdict = validateRequestInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    await this.ensureCollectionsDeclared();
    const request: VouchRequest = {
      requestId: newId('req'),
      context: input.context.trim(),
      subjectRootId: input.subjectRootId.trim(),
      requiredCount: input.requiredCount,
      note: input.note.trim(),
      createdAt: Date.now()
    };
    await this.sdk.docs.put(
      VOUCH_COLLECTIONS.requests,
      request.requestId,
      request as unknown as Record<string, unknown>
    );
    return request;
  }

  async listRequests(): Promise<VouchRequest[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<VouchRequest>(VOUCH_COLLECTIONS.requests, { limit: 1000 });
    return response.items.map((item) => item.data).sort((a, b) => b.createdAt - a.createdAt);
  }

  async listVouches(requestId: string): Promise<Vouch[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<Vouch>(VOUCH_COLLECTIONS.vouches, {
      filter: [{ field: 'requestId', value: requestId }],
      limit: 1000
    });
    return response.items.map((item) => item.data).sort((a, b) => a.vouchedAt - b.vouchedAt);
  }

  async listProofs(): Promise<ThresholdProof[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<ThresholdProof>(VOUCH_COLLECTIONS.proofs, { limit: 1000 });
    return response.items.map((item) => item.data).sort((a, b) => b.assembledAt - a.assembledAt);
  }

  /** 同一担保人重复担保时保留最早一份（一人一票，重复只计一次） */
  async listDistinctVouches(requestId: string): Promise<Vouch[]> {
    const byVoucher = new Map<string, Vouch>();
    for (const vouch of await this.listVouches(requestId)) {
      if (!byVoucher.has(vouch.voucherRootId)) {
        byVoucher.set(vouch.voucherRootId, vouch);
      }
    }
    return [...byVoucher.values()];
  }

  /**
   * 签名担保（担保人侧）：identity:sign 签名，载荷绑定请求/上下文/被担保人/
   * 担保人。签名主体是插件域身份（不证明担保人个人身份，voucherRootId 为
   * 自报文本——见文件头诚实口径）。签名是担保的全部意义——拒绝授权时报错，
   * 不产出无签名担保。
   */
  async addVouch(request: VouchRequest, voucherRootId: string): Promise<Vouch> {
    await this.ensureCollectionsDeclared();
    const payload = buildVouchPayload(request.requestId, request.context, request.subjectRootId, voucherRootId);
    const signed = await this.sdk.identity.sign(payload);
    const vouch: Vouch = {
      requestId: request.requestId,
      voucherRootId,
      payload,
      signature: signed.signature,
      publicKey: signed.publicKey,
      vouchedAt: Date.now()
    };
    await this.sdk.docs.put(VOUCH_COLLECTIONS.vouches, newId('vouch'), vouch as unknown as Record<string, unknown>);
    return vouch;
  }

  /**
   * 组装门槛证明（任一持有者皆可组装——证明是数据，不是特权）：
   * 门槛满足性以「不同担保人计数」判定，组装人再签一次绑定整个担保集合
   * （签名主体同为插件域身份，assembledBy 为自报文本）。
   */
  async assembleProof(request: VouchRequest, assemblerRootId: string): Promise<ThresholdProof> {
    await this.ensureCollectionsDeclared();
    const vouches = await this.listDistinctVouches(request.requestId);
    assertAssemblable(request, vouches);

    const proof: ThresholdProof = {
      proofId: newId('proof'),
      requestId: request.requestId,
      context: request.context,
      subjectRootId: request.subjectRootId,
      requiredCount: request.requiredCount,
      vouches,
      assembledBy: assemblerRootId,
      assembledAt: Date.now(),
      payload: '',
      signature: '',
      publicKey: ''
    };
    const payload = buildProofPayload(proof.proofId, proof.requestId, proof.context, proof.subjectRootId, proof.vouches);
    const signed = await this.sdk.identity.sign(payload);
    proof.payload = payload;
    proof.signature = signed.signature;
    proof.publicKey = signed.publicKey;

    await this.sdk.docs.put(VOUCH_COLLECTIONS.proofs, proof.proofId, proof as unknown as Record<string, unknown>);
    return proof;
  }

  /**
   * 验证证明产物（免权限路径：identity.verify 是纯函数，任何节点可验）。
   * 逐项重算载荷比对再验签——搬用/替换任一份担保或证明字段都会失配。
   * 返回结构化结论而非布尔，验签方如实呈现每一项。
   * 注意：验签通过只证明「签名出自证明所载公钥对应的插件域私钥且载荷
   * 未被改动」，不证明担保人/组装人的个人身份（评审阻塞 3 降级口径）。
   */
  async verifyProof(proof: ThresholdProof): Promise<{
    valid: boolean;
    checks: Array<{ name: string; ok: boolean; detail?: string }>;
  }> {
    const checks: Array<{ name: string; ok: boolean; detail?: string }> = [];

    const expectedProofPayload = buildProofPayload(proof.proofId, proof.requestId, proof.context, proof.subjectRootId, proof.vouches);
    checks.push({
      name: '证明载荷完整性',
      ok: proof.payload === expectedProofPayload,
      detail: proof.payload === expectedProofPayload ? undefined : '证明字段或担保集合被改动'
    });
    const proofSig = await this.sdk.identity.verify(expectedProofPayload, proof.signature, proof.publicKey);
    checks.push({ name: '组装人签名', ok: proofSig.valid });

    const duplicate = countDistinctVouchers(proof.vouches) !== proof.vouches.length;
    checks.push({ name: '担保人无重复', ok: !duplicate, detail: duplicate ? '同一担保人出现多次' : undefined });

    for (const vouch of proof.vouches) {
      const expected = buildVouchPayload(proof.requestId, proof.context, proof.subjectRootId, vouch.voucherRootId);
      if (vouch.payload !== expected) {
        checks.push({ name: `担保载荷 ${vouch.voucherRootId}`, ok: false, detail: '载荷与证明字段不符' });
        continue;
      }
      const verdict = await this.sdk.identity.verify(expected, vouch.signature, vouch.publicKey);
      checks.push({ name: `担保签名 ${vouch.voucherRootId}`, ok: verdict.valid });
    }

    const met = isThresholdMet(proof.vouches, proof.requiredCount, proof.subjectRootId);
    checks.push({
      name: '门槛满足',
      ok: met,
      detail: `${countDistinctVouchers(proof.vouches.filter((v) => v.voucherRootId !== proof.subjectRootId))}/${proof.requiredCount}`
    });

    return { valid: checks.every((check) => check.ok), checks };
  }
}
