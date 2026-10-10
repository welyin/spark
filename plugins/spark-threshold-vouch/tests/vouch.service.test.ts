import { describe, expect, it, vi } from 'vitest';
import { VouchService, VOUCH_COLLECTIONS } from '../service';
import { buildProofPayload, buildVouchPayload, type VouchRequest } from '../model';

/** mock SDK：内存 docs + 可控 identity.sign/verify + runtime.currentRoot（对齐 spark-example mock 形态） */
function createMockSdk() {
  const store = new Map<string, Map<string, Record<string, unknown>>>();
  const docs = {
    defineCollection: vi.fn().mockResolvedValue({
      collection: 'mock',
      syncStrategy: 'append-only',
      governance: false,
      enableEvidence: true
    }),
    get: vi.fn(),
    put: vi.fn().mockImplementation((collection: string, id: string, doc: Record<string, unknown>) => {
      const bucket = store.get(collection) ?? new Map<string, Record<string, unknown>>();
      bucket.set(id, doc);
      store.set(collection, bucket);
      return Promise.resolve({ success: true });
    }),
    delete: vi.fn(),
    query: vi.fn().mockImplementation((collection: string, options?: { filter?: Array<{ field: string; value: string }> }) => {
      const bucket = store.get(collection) ?? new Map<string, Record<string, unknown>>();
      let items = [...bucket.entries()].map(([id, data]) => ({ id, data }));
      const filter = options?.filter?.[0];
      if (filter) {
        items = items.filter((item) => (item.data as Record<string, unknown>)[filter.field] === filter.value);
      }
      return Promise.resolve({ items, nextCursor: undefined });
    })
  };
  const sdk = {
    runtime: {
      currentRoot: vi.fn().mockResolvedValue({ rootId: 'voucher-1', unlocked: true })
    },
    docs,
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig-signed', publicKey: 'pk-1', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    }
  };
  return { sdk: sdk as any, docs, store };
}

/** 以指定身份担保一次：currentRoot 给展示 rootId，identity.sign 给该身份的域公钥（计数基准） */
async function vouchAs(sdk: any, service: VouchService, request: VouchRequest, rootId: string, publicKey: string) {
  sdk.runtime.currentRoot.mockResolvedValueOnce({ rootId, unlocked: true });
  sdk.identity.sign.mockResolvedValueOnce({ signature: `sig-${publicKey}`, publicKey, payloadHash: 'ph' });
  return service.addVouch(request);
}

describe('spark-threshold-vouch service', () => {
  it('declares append-only collections before first write', async () => {
    const { sdk, docs } = createMockSdk();
    const service = new VouchService(sdk);

    await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });

    const declared = docs.defineCollection.mock.calls.map((call: any[]) => call[0]);
    expect(declared).toEqual([VOUCH_COLLECTIONS.requests, VOUCH_COLLECTIONS.vouches, VOUCH_COLLECTIONS.proofs]);
  });

  it('rejects free-text governance context (restricted lexicon affair:/org:)', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);

    await expect(
      service.createRequest({ context: 'ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' })
    ).rejects.toThrow(/affair:<事务id> 或 org:<组织id>/);
  });

  it('vouches as the current identity: rootId from currentRoot (display), publicKey from the signature', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });

    const vouch = await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');

    // 担保人身份不经调用方传入：rootId 取 currentRoot，载荷随之绑定
    expect(sdk.runtime.currentRoot).toHaveBeenCalled();
    expect(sdk.identity.sign).toHaveBeenCalledWith(buildVouchPayload(request.requestId, 'org:ctx-1', 'subject-1', 'voucher-1'));
    expect(vouch.voucherRootId).toBe('voucher-1');
    expect(vouch.publicKey).toBe('pk-v1');
  });

  it('refuses to assemble proof before the threshold is met (counted by distinct publicKeys)', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');

    await expect(service.assembleProof(request, 'me')).rejects.toThrow(/担保不足/);

    // 同一操作者再签一份（同一域公钥）仍只计一次——伪造文本凑数不成立
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await expect(service.assembleProof(request, 'me')).rejects.toThrow(/担保不足/);
  });

  it('assembles signed proof once threshold met (self-vouch excluded)', async () => {
    const { sdk, store } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });
    await vouchAs(sdk, service, request, 'subject-1', 'pk-subject'); // 自查担保不计
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await vouchAs(sdk, service, request, 'voucher-2', 'pk-v2');

    const proof = await service.assembleProof(request, 'me');

    expect(proof.vouches).toHaveLength(3);
    expect(sdk.identity.sign).toHaveBeenLastCalledWith(
      buildProofPayload(proof.proofId, request.requestId, 'org:ctx-1', 'subject-1', proof.vouches)
    );
    expect(store.get(VOUCH_COLLECTIONS.proofs)?.get(proof.proofId)).toBeDefined();
  });

  it('dedups repeated signer publicKeys when listing distinct set', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await vouchAs(sdk, service, request, 'voucher-2', 'pk-v2');

    const distinct = await service.listDistinctVouches(request.requestId);
    expect(distinct.map((v) => v.voucherRootId)).toEqual(['voucher-1', 'voucher-2']);
  });

  it('verifies proof product: recomputed payloads, per-vouch signatures, threshold', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await vouchAs(sdk, service, request, 'voucher-2', 'pk-v2');
    const proof = await service.assembleProof(request, 'me');

    const result = await service.verifyProof(proof);
    expect(result.valid).toBe(true);
    expect(result.checks.map((c) => c.name)).toEqual([
      '证明载荷完整性',
      '组装人签名',
      '担保人无重复',
      '担保签名 voucher-1',
      '担保签名 voucher-2',
      '门槛满足'
    ]);
    // 每份担保的验签都收到重算载荷（免权限 identity.verify）
    expect(sdk.identity.verify).toHaveBeenCalledWith(
      buildVouchPayload(request.requestId, 'org:ctx-1', 'subject-1', 'voucher-1'),
      expect.any(String),
      'pk-v1'
    );
  });

  it('fails verification when a vouch payload is swapped after assembly', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 2, note: '' });
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    await vouchAs(sdk, service, request, 'voucher-2', 'pk-v2');
    const proof = await service.assembleProof(request, 'me');

    // 组装后偷换一份担保人的身份：该份担保的载荷与字段失配（证明载荷
    // 按担保载荷集合哈希，未变）→ 整体无效，失配落在担保载荷检查上
    const tampered = {
      ...proof,
      vouches: proof.vouches.map((vouch, index) => (index === 0 ? { ...vouch, voucherRootId: 'attacker' } : vouch))
    };
    const result = await service.verifyProof(tampered);
    expect(result.valid).toBe(false);
    expect(result.checks.find((c) => c.name === '证明载荷完整性')?.ok).toBe(true);
    expect(result.checks.find((c) => c.name === '担保载荷 attacker')?.ok).toBe(false);
  });

  it('fails verification when a signature does not verify', async () => {
    const { sdk } = createMockSdk();
    const service = new VouchService(sdk);
    const request: VouchRequest = await service.createRequest({ context: 'org:ctx-1', subjectRootId: 'subject-1', requiredCount: 1, note: '' });
    await vouchAs(sdk, service, request, 'voucher-1', 'pk-v1');
    const proof = await service.assembleProof(request, 'me');

    sdk.identity.verify.mockResolvedValueOnce({ valid: true }); // 证明签名
    sdk.identity.verify.mockResolvedValueOnce({ valid: false }); // 担保签名
    const result = await service.verifyProof(proof);
    expect(result.valid).toBe(false);
    expect(result.checks.find((c) => c.name === '担保签名 voucher-1')?.ok).toBe(false);
  });
});
