import { describe, expect, it } from 'vitest';
import {
  assertNoSensitiveFields,
  buildCredentialSignPayload,
  buildRevocationSignPayload,
  deriveApplicationStatus,
  hashMaterialContent,
  validateApplicationInput,
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

  it('hashes material content deterministically', () => {
    expect(hashMaterialContent('abc')).toBe(hashMaterialContent('abc'));
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
});
