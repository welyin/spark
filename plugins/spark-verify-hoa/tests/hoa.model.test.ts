import { describe, expect, it } from 'vitest';
import {
  assertNoSensitiveFields,
  buildCredentialSignPayload,
  buildRevocationSignPayload,
  deriveApplicationStatus,
  hashMaterialContent,
  methodPatternMatches,
  validateApplicationInput,
  verifierGrantCovers,
  type CredentialRevocation,
  type HoaCredential,
  type VerificationApplication
} from '../model';

function validInput() {
  return {
    method: 'deed-manual' as const,
    credentialType: 'owner' as const,
    unitNo: '3-502',
    materials: [{ label: '房产证照片', content: '模拟材料内容' }]
  };
}

describe('spark-verify-hoa model', () => {
  it('validates application input (unitNo required, at least one material)', () => {
    expect(validateApplicationInput(validInput()).ok).toBe(true);
    expect(validateApplicationInput({ ...validInput(), unitNo: ' ' })).toEqual({
      ok: false,
      reason: '户号不能为空'
    });
    expect(validateApplicationInput({ ...validInput(), materials: [] })).toEqual({
      ok: false,
      reason: '至少提交一份材料'
    });
    expect(validateApplicationInput({ ...validInput(), materials: [{ label: '', content: 'x' }] }).ok).toBe(false);
  });

  it('hashes material content with SHA-256 (known vectors, deterministic across nodes)', () => {
    expect(hashMaterialContent('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(hashMaterialContent('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    // 多字节 UTF-8 口径（中文按 UTF-8 字节哈希，任何节点复算一致）
    expect(hashMaterialContent('已出售')).toBe(hashMaterialContent('已出售'));
    expect(hashMaterialContent('abc')).not.toBe(hashMaterialContent('abd'));
  });

  it('bans sensitive fields anywhere in synced records (evidence minimization)', () => {
    expect(() => assertNoSensitiveFields({ unitNo: '3-502', contact: { phone: '138' } })).toThrow(/敏感字段/);
    expect(() => assertNoSensitiveFields({ list: [{ idCardNo: 'x' }] })).toThrow(/敏感字段/);
    expect(() => assertNoSensitiveFields({ unitNo: '3-502', materials: [{ label: '房产证' }] })).not.toThrow();
  });

  it('builds credential payload binding org/subject/type/unitNo/method', () => {
    const payload = buildCredentialSignPayload('org-1', 'cred-1', 'root-a', 'owner', '3-502', 'deed-manual');
    expect(payload).toBe('credential:org-1:cred-1:root-a:owner:3-502:deed-manual');
    expect(payload).not.toBe(buildCredentialSignPayload('org-1', 'cred-1', 'root-a', 'resident', '3-502', 'deed-manual'));
  });

  it('builds revocation payload binding credential and reason hash', () => {
    expect(buildRevocationSignPayload('cred-1', '已出售')).toBe(`revocation:cred-1:${hashMaterialContent('已出售')}`);
  });

  it('derives application status from append-only credential/revocation records', () => {
    const application: VerificationApplication = {
      applicationId: 'app-1',
      orgId: 'org-1',
      applicantRootId: 'root-a',
      method: 'deed-manual',
      credentialType: 'owner',
      unitNo: '3-502',
      materials: [],
      createdAt: 1
    };
    expect(deriveApplicationStatus(application, [], [])).toBe('pending');

    const credential: HoaCredential = {
      credentialId: 'cred-1',
      applicationId: 'app-1',
      orgId: 'org-1',
      subjectRootId: 'root-a',
      credentialType: 'owner',
      unitNo: '3-502',
      method: 'deed-manual',
      issuerRootId: 'root-v',
      issuedAt: 2,
      signature: { payload: 'p', signature: 's', publicKey: 'k' }
    };
    expect(deriveApplicationStatus(application, [credential], [])).toBe('issued');

    const revocation: CredentialRevocation = {
      revocationId: 'rev-1',
      credentialId: 'cred-1',
      reason: '房屋已出售',
      revokedBy: 'root-v',
      revokedAt: 3,
      signature: { payload: 'p', signature: 's', publicKey: 'k' }
    };
    expect(deriveApplicationStatus(application, [credential], [revocation])).toBe('revoked');
  });

  it('matches method patterns with trailing-* prefix wildcard (kernel method_matches semantics)', () => {
    expect(methodPatternMatches('vouch-*', 'vouch-2')).toBe(true);
    expect(methodPatternMatches('vouch-*', 'vouch-')).toBe(true);
    expect(methodPatternMatches('*', 'deed-manual')).toBe(true);
    expect(methodPatternMatches('deed-manual', 'deed-manual')).toBe(true);
    expect(methodPatternMatches('vouch-*', 'deed-manual')).toBe(false);
    expect(methodPatternMatches('deed-manual', 'deed-manual-2')).toBe(false);
    expect(methodPatternMatches('deed-*manual', 'deed-manual')).toBe(false); // 仅尾部 * 是通配
  });

  it('verifierGrantCovers requires identity + exact credType + method pattern (kernel verifier_granted semantics)', () => {
    const grants = [
      { identity: 'root-v', credTypes: ['owner'], methods: ['deed-manual', 'vouch-*'] },
      { identity: 'root-w', credTypes: ['resident'], methods: ['gov-realname'] }
    ];
    expect(verifierGrantCovers(grants, 'root-v', 'owner', 'deed-manual')).toBe(true);
    expect(verifierGrantCovers(grants, 'root-v', 'owner', 'vouch-2')).toBe(true);
    expect(verifierGrantCovers(grants, 'root-w', 'resident', 'gov-realname')).toBe(true);
    // 身份不在声明内 / credType 超范围 / method 超范围 / 空声明一律不覆盖
    expect(verifierGrantCovers(grants, 'root-x', 'owner', 'deed-manual')).toBe(false);
    expect(verifierGrantCovers(grants, 'root-v', 'resident', 'deed-manual')).toBe(false);
    expect(verifierGrantCovers(grants, 'root-v', 'owner', 'gov-realname')).toBe(false);
    expect(verifierGrantCovers([], 'root-v', 'owner', 'deed-manual')).toBe(false);
  });
});
