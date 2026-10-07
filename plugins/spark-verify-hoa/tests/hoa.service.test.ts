import { describe, expect, it, vi } from 'vitest';
import { HoaVerifyService, HOA_COLLECTIONS } from '../service';
import { CREDENTIALS_MODULE_MISSING } from '../sdk-credentials';
import {
  buildCredentialSignPayload,
  hashMaterialContent,
  type HoaCredential,
  type VerificationApplication
} from '../model';

/**
 * mock SDK：docs 为内存实现（保留 put 载荷供断言），identity/messages 对齐
 * spark-example 的 mock 形态；credentials 模块按已落地 SDK 面
 * （plugin-sdk PluginCredentialsAPI：listHeld / presentHolderProof /
 * queryVerifiers / verify / queryRevocations）实现——钉住真实契约，
 * 接口错位会在测试里立刻暴露。
 */
function createMockSdk(withCredentialsModule = false) {
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
    query: vi.fn().mockImplementation((collection: string) => {
      const bucket = store.get(collection) ?? new Map<string, Record<string, unknown>>();
      return Promise.resolve({ items: [...bucket.entries()].map(([id, data]) => ({ id, data })), nextCursor: undefined });
    })
  };
  const sdk: Record<string, unknown> = {
    docs,
    identity: {
      sign: vi.fn().mockResolvedValue({
        domain: 'plugin:spark-verify-hoa',
        domainId: 'spark-verify-hoa',
        publicKey: 'pk-1',
        signature: 'sig-1',
        payloadHash: 'ph-1'
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    }
  };
  if (withCredentialsModule) {
    sdk.credentials = {
      listHeld: vi.fn().mockResolvedValue([]),
      presentHolderProof: vi.fn().mockResolvedValue({
        credential: { credV: 1 },
        holderProof: { credId: 'cred-held-1', sig: 'proof-sig' },
        presentedAt: 1000
      }),
      queryVerifiers: vi.fn().mockResolvedValue({ orgId: 'org-1', effectiveFrom: 0, seq: 1, updatedAt: 0, verifiers: [] }),
      verify: vi.fn().mockResolvedValue({
        credId: 'cred-held-1',
        valid: true,
        checks: { static: true, trust: true, revocation: 'not-revoked' },
        reason: null
      }),
      queryRevocations: vi.fn().mockResolvedValue({ issuer: 'ab'.repeat(32), available: false })
    };
  }
  return { sdk: sdk as any, docs, store };
}

function pendingApplication(): VerificationApplication {
  return {
    applicationId: 'app-1',
    orgId: 'org-1',
    applicantRootId: 'root-a',
    method: 'deed-manual',
    credentialType: 'owner',
    unitNo: '3-502',
    materials: [{ label: '房产证照片', contentHash: hashMaterialContent('m'), byteSize: 1 }],
    createdAt: 1
  };
}

/** 信任声明中覆盖 root-verifier 的授权条目（credTypes 精确 + methods 含前缀通配） */
const VERIFIER_GRANT = {
  identity: 'root-verifier',
  publicKey: 'pk-verifier',
  credTypes: ['owner', 'resident'],
  methods: ['deed-manual', 'vouch-*', 'gov-realname']
};

/** 让 mock 的 queryVerifiers 返回给定授权条目集（默认覆盖 root-verifier） */
function mockTrustDecl(sdk: any, verifiers: unknown[] = [VERIFIER_GRANT]) {
  sdk.credentials.queryVerifiers.mockResolvedValue({
    orgId: 'org-1',
    effectiveFrom: 0,
    seq: 1,
    updatedAt: 0,
    verifiers
  });
}

describe('spark-verify-hoa service: applications & demo credentials', () => {
  it('declares append-only collections before first write', async () => {
    const { sdk, docs } = createMockSdk();
    const service = new HoaVerifyService(sdk);

    await service.submitApplication('org-1', 'root-a', {
      method: 'deed-manual',
      credentialType: 'owner',
      unitNo: '3-502',
      materials: [{ label: '房产证照片', content: '内容' }]
    });

    const declared = docs.defineCollection.mock.calls.map((call: any[]) => call[0]);
    for (const collection of [
      HOA_COLLECTIONS.applications,
      HOA_COLLECTIONS.credentials,
      HOA_COLLECTIONS.revocations,
      HOA_COLLECTIONS.materials
    ]) {
      expect(declared).toContain(collection);
    }
    // 声明幂等
    await service.listApplications('org-1');
    expect(docs.defineCollection.mock.calls.length).toBe(4);
  });

  it('stores only material summaries in the synced collection; raw content stays local (__sync:false)', async () => {
    const { sdk, store } = createMockSdk();
    const service = new HoaVerifyService(sdk);

    const application = await service.submitApplication('org-1', 'root-a', {
      method: 'vouch-2',
      credentialType: 'resident',
      unitNo: '1-101',
      materials: [{ label: '租赁合同', content: '租约原文' }]
    });

    const synced = store.get(HOA_COLLECTIONS.applications)?.get(application.applicationId);
    expect(synced).toBeDefined();
    expect(JSON.stringify(synced)).not.toContain('租约原文');
    expect((synced?.materials as Array<{ contentHash: string }>)[0].contentHash).toBe(
      hashMaterialContent('租约原文')
    );

    const local = store.get(HOA_COLLECTIONS.materials)?.get(`${application.applicationId}:0`);
    expect(local?.content).toBe('租约原文');
    expect(local?.__sync).toBe(false);
  });

  it('keeps material raw content (even names) out of the synced application record', async () => {
    const { sdk } = createMockSdk();
    const service = new HoaVerifyService(sdk);

    // 原文可含姓名（只给验证人看）；同步集合里只允许出现摘要
    const application = await service.submitApplication('org-1', 'root-a', {
      method: 'deed-manual',
      credentialType: 'owner',
      unitNo: '3-502',
      materials: [{ label: '房产证', content: '持有人：张三' }]
    });

    expect(application.materials[0].contentHash).toBe(hashMaterialContent('持有人：张三'));
    expect(JSON.stringify(application)).not.toContain('张三');
  });

  it('issues demo credential signed with the plugin domain identity (payload binds qualification fields)', async () => {
    const { sdk, store } = createMockSdk(true);
    mockTrustDecl(sdk);
    const service = new HoaVerifyService(sdk);

    const credential = await service.issueCredential(pendingApplication(), 'root-verifier');

    expect(sdk.credentials.queryVerifiers).toHaveBeenCalledWith('org-1');
    expect(sdk.identity.sign).toHaveBeenCalledWith(
      buildCredentialSignPayload('org-1', credential.credentialId, 'root-a', 'owner', '3-502', 'deed-manual')
    );
    expect(credential.signature).toEqual({ payload: expect.any(String), signature: 'sig-1', publicKey: 'pk-1' });
    expect(store.get(HOA_COLLECTIONS.credentials)?.get(credential.credentialId)).toBeDefined();
  });

  it('does not issue unsigned credential when identity:sign is rejected (signature IS the product)', async () => {
    const { sdk } = createMockSdk(true);
    mockTrustDecl(sdk);
    sdk.identity.sign.mockRejectedValueOnce(new Error('Access denied: identity:sign rejected by user'));
    const service = new HoaVerifyService(sdk);

    await expect(service.issueCredential(pendingApplication(), 'root-verifier')).rejects.toThrow(/identity:sign/);
  });

  it('rejects issue when the current identity is not in the verifier trust declaration (soft gate)', async () => {
    const { sdk, store } = createMockSdk(true);
    mockTrustDecl(sdk); // 声明里只有 root-verifier
    const service = new HoaVerifyService(sdk);

    await expect(service.issueCredential(pendingApplication(), 'root-stranger')).rejects.toThrow(
      /不在本组织验证人信任声明内/
    );
    expect(sdk.identity.sign).not.toHaveBeenCalled();
    expect(store.get(HOA_COLLECTIONS.credentials)?.size ?? 0).toBe(0);
  });

  it('rejects issue when credType/method is outside the granted scope (soft gate)', async () => {
    const { sdk } = createMockSdk(true);
    // 身份在声明内，但授权范围只含 resident + gov-realname，不覆盖 owner/deed-manual
    mockTrustDecl(sdk, [
      { identity: 'root-verifier', publicKey: 'pk-verifier', credTypes: ['resident'], methods: ['gov-realname'] }
    ]);
    const service = new HoaVerifyService(sdk);

    await expect(service.issueCredential(pendingApplication(), 'root-verifier')).rejects.toThrow(
      /授权范围不含 owner\/deed-manual/
    );
    expect(sdk.identity.sign).not.toHaveBeenCalled();
  });

  it('rejects issue fail-closed when sdk.credentials is unavailable (gate cannot be evaluated)', async () => {
    const { sdk } = createMockSdk(); // 无 credentials 模块：软闸门无从核对，不得静默放行
    const service = new HoaVerifyService(sdk);

    await expect(service.issueCredential(pendingApplication(), 'root-verifier')).rejects.toThrow(/sdk\.credentials/);
    expect(sdk.identity.sign).not.toHaveBeenCalled();
  });

  it('revokes credential with signed revocation record', async () => {
    const { sdk, store } = createMockSdk(true);
    mockTrustDecl(sdk);
    const service = new HoaVerifyService(sdk);
    const credential = await service.issueCredential(pendingApplication(), 'root-verifier');

    const revocation = await service.revokeCredential(credential, '房屋已出售', 'root-verifier');

    expect(revocation.credentialId).toBe(credential.credentialId);
    expect(store.get(HOA_COLLECTIONS.revocations)?.get(revocation.revocationId)).toBeDefined();
    await expect(service.revokeCredential(credential, '   ', 'root-verifier')).rejects.toThrow(/理由不能为空/);
  });

  it('rejects revoke when the current identity is not a granted verifier (soft gate)', async () => {
    const { sdk, store } = createMockSdk(true);
    mockTrustDecl(sdk);
    const service = new HoaVerifyService(sdk);
    const credential = await service.issueCredential(pendingApplication(), 'root-verifier');

    await expect(service.revokeCredential(credential, '房屋已出售', 'root-stranger')).rejects.toThrow(
      /不具备注销凭证资格/
    );
    expect(store.get(HOA_COLLECTIONS.revocations)?.size ?? 0).toBe(0);
  });
});

describe('spark-verify-hoa service: kernel credential surface (sdk.credentials read-only)', () => {
  it('delegates listHeld / presentHolderProof / queryVerifiers to the landed SDK surface', async () => {
    const { sdk } = createMockSdk(true);
    const service = new HoaVerifyService(sdk);

    await expect(service.listHeldCredentials()).resolves.toEqual([]);
    const presentation = await service.presentHeldProof({
      credId: 'cred-held-1',
      requestId: 'req-1',
      orgId: 'org-1',
      collection: 'members'
    });
    expect(presentation.holderProof).toEqual({ credId: 'cred-held-1', sig: 'proof-sig' });
    expect(sdk.credentials.presentHolderProof).toHaveBeenCalledWith({
      credId: 'cred-held-1',
      requestId: 'req-1',
      orgId: 'org-1',
      collection: 'members'
    });
    await expect(service.queryVerifiers('org-1')).resolves.toMatchObject({ orgId: 'org-1', verifiers: [] });
  });

  it('delegates verify / queryRevocations to the kernel verification chain (no local stand-in)', async () => {
    const { sdk } = createMockSdk(true);
    const service = new HoaVerifyService(sdk);

    // 协议线形凭证的验证走内核链（结构化裁决原样透传）
    const credential = { credV: 1, credType: 'owner' } as any;
    const verdict = await service.verifyProtocolCredential(credential);
    expect(verdict.valid).toBe(true);
    expect(verdict.checks).toEqual({ static: true, trust: true, revocation: 'not-revoked' });
    expect(sdk.credentials.verify).toHaveBeenCalledWith(credential);

    // 注销快照：available:false 如实透传（不冒充「无注销」）
    const snapshot = await service.queryRevocationSnapshot('ab'.repeat(32));
    expect(snapshot).toEqual({ issuer: 'ab'.repeat(32), available: false });
    expect(sdk.credentials.queryRevocations).toHaveBeenCalledWith('ab'.repeat(32));
  });

  it('fails fast naming the missing method when the module shape is off the landed SDK surface', async () => {
    const { sdk } = createMockSdk(true);
    delete sdk.credentials.presentHolderProof;
    const service = new HoaVerifyService(sdk);

    await expect(
      service.presentHeldProof({ credId: 'c', requestId: 'r', orgId: 'o', collection: 'members' })
    ).rejects.toThrow(/presentHolderProof/);
    expect(HoaVerifyService.isAvailable(sdk)).toBe(false);
  });

  it('surfaces presentation failures truthfully (no silent permission-degradation swallow)', async () => {
    const { sdk } = createMockSdk(true);
    // 桥层真实错误形态：unknown-call / Access denied 都必须如实上抛
    sdk.credentials.presentHolderProof.mockRejectedValueOnce(new Error('unknown SDK call credentials.presentHolderProof'));
    sdk.credentials.presentHolderProof.mockRejectedValueOnce(new Error('Access denied: credentials:read not granted'));
    const service = new HoaVerifyService(sdk);

    await expect(
      service.presentHeldProof({ credId: 'c', requestId: 'r', orgId: 'o', collection: 'members' })
    ).rejects.toThrow(/unknown SDK call/);
    await expect(
      service.presentHeldProof({ credId: 'c', requestId: 'r', orgId: 'o', collection: 'members' })
    ).rejects.toThrow(/Access denied/);
  });

  it('throws CREDENTIALS_MODULE_MISSING without sdk.credentials (no silent fallback)', async () => {
    const { sdk } = createMockSdk();
    const service = new HoaVerifyService(sdk);

    await expect(service.listHeldCredentials()).rejects.toThrow(CREDENTIALS_MODULE_MISSING);
    await expect(
      service.presentHeldProof({ credId: 'c', requestId: 'r', orgId: 'o', collection: 'members' })
    ).rejects.toThrow(/sdk\.credentials/);
    await expect(service.queryVerifiers('org-1')).rejects.toThrow(/sdk\.credentials/);
    await expect(service.verifyProtocolCredential({ credV: 1 } as never)).rejects.toThrow(/sdk\.credentials/);
    await expect(service.queryRevocationSnapshot('ab'.repeat(32))).rejects.toThrow(/sdk\.credentials/);
    expect(HoaVerifyService.isAvailable(sdk)).toBe(false);
  });
});

describe('spark-verify-hoa service: demo-grade self-check (HoaCredential is NOT a protocol credential)', () => {
  it('self-checks locally: recomputed payload + identity.verify + local revocation list, honestly labeled', async () => {
    const { sdk } = createMockSdk(true); // 即使有 credentials 模块，演示凭证也只能本地自查（演示线形不过内核验证链）
    mockTrustDecl(sdk);
    const service = new HoaVerifyService(sdk);
    const credential = await service.issueCredential(pendingApplication(), 'root-verifier');

    const ok = await service.verifyCredential(credential);
    expect(ok.valid).toBe(true);
    expect(ok.via).toBe('local-demo');
    expect(ok.reason).toContain('插件域身份');
    expect(ok.reason).toContain('信任链');

    // 字段被搬用替换：重算载荷失配，不进入密码学验签
    const tampered: HoaCredential = { ...credential, unitNo: '3-503' };
    const bad = await service.verifyCredential(tampered);
    expect(bad.valid).toBe(false);
    expect(bad.reason).toContain('不符');

    // 注销后本地自查判无效
    await service.revokeCredential(credential, '房屋已出售', 'root-verifier');
    const revoked = await service.verifyCredential(credential);
    expect(revoked.valid).toBe(false);
    expect(revoked.reason).toContain('注销');
  });
});
