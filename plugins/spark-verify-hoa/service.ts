/**
 * 业主资格验证示例（spark-verify-hoa）· 业务服务层。
 *
 * 职责边界（community-model.md §十 + 已落地 SDK 面）：
 * - 申请人侧：材料提交引导（校验 + 敏感字段禁令 + 材料摘要入同步集合）；
 * - 验证人侧：审核申请、签发凭证（identity:sign）、主动注销；签发/注销
 *   前置验证人软闸门——queryVerifiers 核对当前身份在组织信任声明内且
 *   credType/method 在授权范围，不命中则拒绝（流程层防误操作，非密码学
 *   约束，见 assertVerifierGranted）；
 * - 内核凭证面：sdk.credentials 只读五方法（listHeld / presentHolderProof /
 *   queryVerifiers / verify / queryRevocations，credentials:read，对接见
 *   sdk-credentials.ts）。协议线形凭证的验证与注销查询走内核验证链，
 *   不用本地顶替；本插件签发的演示级线形不过内核链（见下诚实口径）。
 *
 * 诚实口径（评审阻塞 3/4 的降级处理，README 同步声明）：
 * - 平台 SDK 只有 identity.sign（插件域身份签名，无个人身份签名面），故签发/
 *   注销签名的主体是本插件域身份（plugin:spark-verify-hoa 派生钥匙），密码学上
 *   不证明验证人个人身份；issuerRootId / revokedBy 为自报文本；
 * - HoaCredential 是演示级线形（model.ts 头注）：与协议凭证线形
 *   （credential §2）不同，不进 cred:held: 键域、过不了内核验证链，
 *   与内核凭证体系零互操作——verifyCredential 的「验证」只是本插件集合内的
 *   演示级自查（重算载荷 + identity.verify + 本地注销表），不冒充内核验证；
 * - 错误纪律：sdk.credentials 调用的失败（权限拒绝、接口错位、内核校验失败）
 *   一律如实上抛，不按「权限降级」静默吞掉（评审阻塞 2 的错误吞噬路径已拆）。
 *
 * 数据面（全部 append-only——资格与注销必须留痕可审计，录入人留痕；
 * 记录 id 内容寻址：同一语义内容复算同一 id，见 contentId）：
 * - hoa_applications：申请材料**摘要**（标签+哈希+大小），原文永不同步；
 * - hoa_materials：材料原文，显式 __sync:false 本地留存（证据最小披露）；
 * - hoa_credentials / hoa_revocations：演示级凭证与注销记录（插件域身份签名）。
 */
import type {
  CredentialVerifyResult,
  HeldCredential,
  HolderProofPresentation,
  PluginCredential,
  PluginSDK,
  RevocationSnapshotView,
  VerifierSet
} from '../../packages/plugin-sdk/src';
import { hasCredentialsModule, requireCredentialsModule } from './sdk-credentials';
import {
  assertNoSensitiveFields,
  buildCredentialSignPayload,
  buildRevocationSignPayload,
  hashMaterialContent,
  validateApplicationInput,
  verifierGrantCovers,
  type CredentialRevocation,
  type HoaCredential,
  type MaterialDraft,
  type VerificationApplication,
  type VerificationMethod,
  type HoaCredentialType
} from './model';

export const HOA_COLLECTIONS = {
  applications: 'hoa_applications',
  materials: 'hoa_materials',
  credentials: 'hoa_credentials',
  revocations: 'hoa_revocations'
} as const;

const COLLECTION_SCHEMAS = {
  [HOA_COLLECTIONS.applications]: { syncStrategy: 'append-only' as const },
  [HOA_COLLECTIONS.credentials]: { syncStrategy: 'append-only' as const },
  [HOA_COLLECTIONS.revocations]: { syncStrategy: 'append-only' as const },
  // 材料原文：本地集合（不写同步策略语义，文档级 __sync:false 控制）
  [HOA_COLLECTIONS.materials]: { syncStrategy: 'append-only' as const }
};

/**
 * 内容寻址 id（对齐 credential §2 credId = canonical 复算口径的演示级落法）：
 * 同一语义内容在任何节点复算同一 id，重复提交/重复签发幂等归并到同一条
 * append-only 记录，取代 Date.now+随机的演示 id。
 */
function contentId(prefix: string, canonical: string): string {
  return `${prefix}_${hashMaterialContent(canonical)}`;
}

export class HoaVerifyService {
  private collectionsReady: Promise<void> | null = null;

  constructor(private readonly sdk: PluginSDK) {}

  static isAvailable(sdk: PluginSDK): boolean {
    return hasCredentialsModule(sdk);
  }

  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const [collection, schema] of Object.entries(COLLECTION_SCHEMAS)) {
        await this.sdk.docs.defineCollection(collection, schema);
      }
    })();
    return this.collectionsReady;
  }

  /**
   * 提交验证申请（申请人侧）。
   *
   * 证据最小披露的落法：同步集合里只有材料摘要（标签 + 内容哈希 + 字节数），
   * 原文写入本机本地文档（__sync:false）。验证人审核看到的是摘要；原文的
   * 实际传递发生在人机流程内（线下/加密通道），本示例不承载——这是诚实
   * 边界，不是实现缺口。
   */
  async submitApplication(
    orgId: string,
    applicantRootId: string,
    input: {
      method: VerificationMethod;
      credentialType: HoaCredentialType;
      unitNo: string;
      materials: MaterialDraft[];
    }
  ): Promise<VerificationApplication> {
    const verdict = validateApplicationInput(input);
    if (!verdict.ok) {
      throw new Error(verdict.reason);
    }
    await this.ensureCollectionsDeclared();

    const materials = input.materials.map((material) => ({
      label: material.label.trim(),
      contentHash: hashMaterialContent(material.content),
      byteSize: new Blob([material.content]).size
    }));
    const application: VerificationApplication = {
      applicationId: contentId(
        'app',
        [
          orgId,
          applicantRootId,
          input.method,
          input.credentialType,
          input.unitNo.trim(),
          ...materials.map((material) => `${material.label}:${material.contentHash}:${material.byteSize}`)
        ].join('|')
      ),
      orgId,
      applicantRootId,
      method: input.method,
      credentialType: input.credentialType,
      unitNo: input.unitNo.trim(),
      materials,
      createdAt: Date.now()
    };
    assertNoSensitiveFields(application);

    await this.sdk.docs.put(
      HOA_COLLECTIONS.applications,
      application.applicationId,
      application as unknown as Record<string, unknown>
    );
    for (const [index, material] of input.materials.entries()) {
      await this.sdk.docs.put(HOA_COLLECTIONS.materials, `${application.applicationId}:${index}`, {
        applicationId: application.applicationId,
        label: material.label.trim(),
        content: material.content,
        __sync: false
      });
    }
    return application;
  }

  async listApplications(orgId: string): Promise<VerificationApplication[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<VerificationApplication>(HOA_COLLECTIONS.applications, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 1000
    });
    return response.items.map((item) => item.data).sort((a, b) => b.createdAt - a.createdAt);
  }

  async listCredentials(orgId: string): Promise<HoaCredential[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<HoaCredential>(HOA_COLLECTIONS.credentials, {
      filter: [{ field: 'orgId', value: orgId }],
      limit: 1000
    });
    return response.items.map((item) => item.data).sort((a, b) => b.issuedAt - a.issuedAt);
  }

  async listRevocations(): Promise<CredentialRevocation[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.docs.query<CredentialRevocation>(HOA_COLLECTIONS.revocations, {
      limit: 1000
    });
    return response.items.map((item) => item.data);
  }

  /**
   * 签发凭证（验证人侧）。sdk.credentials 无签发接口（内核凭证体系刻意不对
   * 插件开放）：签发 = 本插件的人机审核流程（线下核对材料）+ identity:sign。
   * 签名主体是插件域身份（不证明验证人个人身份，issuerRootId 为自报文本），
   * 产物是演示级线形（不过内核验证链）——如实标注，不冒充协议凭证。
   * 前置验证人软闸门（见 assertVerifierGranted）：当前身份不在组织信任声明
   * 内或 credType/method 超授权范围时拒绝签发。
   * 用户拒绝签名时直接报错：不产出无签名凭证。
   */
  async issueCredential(application: VerificationApplication, verifierRootId: string): Promise<HoaCredential> {
    await this.ensureCollectionsDeclared();
    await this.assertVerifierGranted(
      application.orgId,
      verifierRootId,
      application.credentialType,
      application.method,
      '签发凭证'
    );
    const credential: HoaCredential = {
      credentialId: contentId(
        'cred',
        [
          application.orgId,
          application.applicationId,
          application.applicantRootId,
          application.credentialType,
          application.unitNo,
          application.method
        ].join('|')
      ),
      applicationId: application.applicationId,
      orgId: application.orgId,
      subjectRootId: application.applicantRootId,
      credentialType: application.credentialType,
      unitNo: application.unitNo,
      method: application.method,
      issuerRootId: verifierRootId,
      issuedAt: Date.now(),
      signature: { payload: '', signature: '', publicKey: '' }
    };
    const payload = buildCredentialSignPayload(
      credential.orgId,
      credential.credentialId,
      credential.subjectRootId,
      credential.credentialType,
      credential.unitNo,
      credential.method
    );
    const signed = await this.sdk.identity.sign(payload);
    credential.signature = { payload, signature: signed.signature, publicKey: signed.publicKey };
    assertNoSensitiveFields(credential);

    await this.sdk.docs.put(
      HOA_COLLECTIONS.credentials,
      credential.credentialId,
      credential as unknown as Record<string, unknown>
    );
    return credential;
  }

  /** 注销凭证（演示级记录；签名主体同为插件域身份，revokedBy 为自报文本）。
   * 前置验证人软闸门（见 assertVerifierGranted）：当前身份不在组织信任声明
   * 内或 credType/method 超授权范围时拒绝注销。 */
  async revokeCredential(credential: HoaCredential, reason: string, verifierRootId: string): Promise<CredentialRevocation> {
    const trimmed = reason.trim();
    if (!trimmed) {
      throw new Error('注销理由不能为空');
    }
    await this.ensureCollectionsDeclared();
    await this.assertVerifierGranted(
      credential.orgId,
      verifierRootId,
      credential.credentialType,
      credential.method,
      '注销凭证'
    );
    const payload = buildRevocationSignPayload(credential.credentialId, trimmed);
    const signed = await this.sdk.identity.sign(payload);
    const revocation: CredentialRevocation = {
      revocationId: contentId('rev', [credential.credentialId, trimmed, verifierRootId].join('|')),
      credentialId: credential.credentialId,
      reason: trimmed,
      revokedBy: verifierRootId,
      revokedAt: Date.now(),
      signature: { payload, signature: signed.signature, publicKey: signed.publicKey }
    };
    await this.sdk.docs.put(
      HOA_COLLECTIONS.revocations,
      revocation.revocationId,
      revocation as unknown as Record<string, unknown>
    );
    return revocation;
  }

  /**
   * 验证人软闸门（签发/注销共用前置）：查询组织验证人信任声明
   * （sdk.credentials.queryVerifiers），要求当前身份命中 verifiers 且
   * credType/method 在其授权范围内（判定口径对齐内核 verifier_granted，
   * 见 model.verifierGrantCovers）；不命中则拒绝。诚实边界：这是流程层
   * 软闸门——防误操作、让「不具备验证人资格」如实呈现，但不构成密码学
   * 约束（签名主体仍是插件域钥匙，防不了恶意绕过）。queryVerifiers 的
   * 失败（权限拒绝/结构损坏）如实上抛，按 fail-closed 处理。
   */
  private async assertVerifierGranted(
    orgId: string,
    verifierRootId: string,
    credentialType: HoaCredentialType,
    method: VerificationMethod,
    action: string
  ): Promise<void> {
    const trust = await requireCredentialsModule(this.sdk).queryVerifiers(orgId);
    if (!verifierGrantCovers(trust.verifiers, verifierRootId, credentialType, method)) {
      throw new Error(
        `当前身份不在本组织验证人信任声明内（或授权范围不含 ${credentialType}/${method}），不具备${action}资格`
      );
    }
  }

  // ------------------------------------------------------------------
  // 内核凭证面（sdk.credentials 只读五方法，credentials:read）
  // ------------------------------------------------------------------

  /** 本机持有的协议凭证（cred:held: 键域；本插件签发的演示凭证不在其中） */
  async listHeldCredentials(): Promise<HeldCredential[]> {
    return requireCredentialsModule(this.sdk).listHeld();
  }

  /**
   * 对持有的协议凭证出示 holderProof（read-gate §3 载荷，域身份由桥按绑定
   * 身份注入）。失败（权限拒绝/结构非法/接口错位）如实上抛——出示失败必须
   * 让用户看到真实原因，不得静默降级为「未完成」。
   */
  async presentHeldProof(input: {
    credId: string;
    requestId: string;
    orgId: string;
    collection: string;
  }): Promise<HolderProofPresentation> {
    return requireCredentialsModule(this.sdk).presentHolderProof(input);
  }

  /** 查询组织的验证人信任声明（缺失返回空集，结构损坏报错上抛） */
  async queryVerifiers(orgId: string): Promise<VerifierSet> {
    return requireCredentialsModule(this.sdk).queryVerifiers(orgId);
  }

  /**
   * 内核验证链（credential §6 第 1–5 步结构化裁决）：仅适用于协议线形凭证
   * （如 listHeld 返回的持有凭证）。valid=true 当且仅当静态链全过、签发人
   * 信任链匹配且注销检查明确通过；逐项失败原因如实回显（结构化返回而非
   * 整体报错）。本插件签发的演示凭证不过此链（线形不同），只能走
   * verifyCredential 本地自查——两者不可混用。
   */
  async verifyProtocolCredential(credential: PluginCredential): Promise<CredentialVerifyResult> {
    return requireCredentialsModule(this.sdk).verify(credential);
  }

  /**
   * 按 issuer identity 查本地注销快照（sdk.credentials.queryRevocations）。
   * 快照缺失如实报 available:false（注销状态未知），不冒充「无注销」——
   * fail-closed 取舍归消费方。
   */
  async queryRevocationSnapshot(issuer: string): Promise<RevocationSnapshotView> {
    return requireCredentialsModule(this.sdk).queryRevocations(issuer);
  }

  /**
   * 演示级自查（仅适用于本插件签发的 HoaCredential）：重算签名载荷比对 +
   * 免权限 identity.verify + 本地注销表核对。如实口径：
   * - 只证明「签名出自 signature.publicKey 对应私钥」（即本插件域身份）；
   * - 不核对签发人信任链（该公钥是否组织信任的验证人）；
   * - 不过内核验证链（HoaCredential 不是协议线形，见 model.ts 头注）。
   */
  async verifyCredential(credential: HoaCredential): Promise<{ valid: boolean; via: 'local-demo'; reason?: string }> {
    const expected = buildCredentialSignPayload(
      credential.orgId,
      credential.credentialId,
      credential.subjectRootId,
      credential.credentialType,
      credential.unitNo,
      credential.method
    );
    if (credential.signature.payload !== expected) {
      return { valid: false, via: 'local-demo', reason: '随凭证载荷与字段不符（疑似搬用/替换）' };
    }
    const verdict = await this.sdk.identity.verify(expected, credential.signature.signature, credential.signature.publicKey);
    if (!verdict.valid) {
      return { valid: false, via: 'local-demo', reason: '签名校验失败' };
    }
    const revocations = await this.listRevocations();
    if (revocations.some((item) => item.credentialId === credential.credentialId)) {
      return { valid: false, via: 'local-demo', reason: '凭证已被注销（本地记录）' };
    }
    return {
      valid: true,
      via: 'local-demo',
      reason: '演示级自查：签名主体为插件域身份，未核对签发人信任链，不过内核验证链'
    };
  }
}
