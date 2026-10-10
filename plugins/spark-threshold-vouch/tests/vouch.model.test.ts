import { describe, expect, it } from 'vitest';
import {
  assertAssemblable,
  buildProofPayload,
  buildVouchPayload,
  countDistinctVouchers,
  hashText,
  isThresholdMet,
  isValidContext,
  validateRequestInput,
  type Vouch,
  type VouchRequest
} from '../model';

function vouchOf(voucherRootId: string, requestId = 'req-1', publicKey = `pk-${voucherRootId}`): Vouch {
  return {
    requestId,
    voucherRootId,
    payload: buildVouchPayload(requestId, 'org:ctx-1', 'subject-1', voucherRootId),
    signature: 'sig',
    publicKey,
    vouchedAt: 1
  };
}

function requestOf(requiredCount = 2): VouchRequest {
  return {
    requestId: 'req-1',
    context: 'org:ctx-1',
    subjectRootId: 'subject-1',
    requiredCount,
    note: '',
    createdAt: 1
  };
}

describe('spark-threshold-vouch model', () => {
  it('validates request input (context lexicon/subject required, N within bounds)', () => {
    expect(validateRequestInput({ context: 'affair:x', subjectRootId: 'root-a', requiredCount: 2, note: '' }).ok).toBe(true);
    expect(validateRequestInput({ context: 'org:space-1', subjectRootId: 'root-a', requiredCount: 1, note: '' }).ok).toBe(true);
    expect(validateRequestInput({ context: ' ', subjectRootId: 'root-a', requiredCount: 2, note: '' })).toEqual({
      ok: false,
      reason: '治理上下文不能为空（affair:<事务id> 或 org:<组织id>）'
    });
    // 受限词法：自由文本 / 空前缀标识段一律拒绝
    expect(validateRequestInput({ context: 'ctx-1', subjectRootId: 'root-a', requiredCount: 2, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'affair:', subjectRootId: 'root-a', requiredCount: 2, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'affair:x', subjectRootId: '', requiredCount: 2, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'affair:x', subjectRootId: 'r', requiredCount: 0, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'affair:x', subjectRootId: 'r', requiredCount: 1.5, note: '' }).ok).toBe(false);
  });

  it('isValidContext enforces the affair:/org: restricted lexicon', () => {
    expect(isValidContext('affair:ab12')).toBe(true);
    expect(isValidContext('org:cd34')).toBe(true);
    expect(isValidContext('affair_xxx')).toBe(false);
    expect(isValidContext('project:space')).toBe(false);
    expect(isValidContext('org:  ')).toBe(false);
  });

  it('builds vouch payload binding request/context/subject/voucher', () => {
    expect(buildVouchPayload('req-1', 'org:ctx-1', 'subject-1', 'voucher-1')).toBe(
      'vouch:req-1:org:ctx-1:subject-1:voucher-1'
    );
    expect(buildVouchPayload('req-1', 'org:ctx-1', 'subject-1', 'voucher-1')).not.toBe(
      buildVouchPayload('req-1', 'org:ctx-1', 'subject-1', 'voucher-2')
    );
  });

  it('counts distinct vouchers by signature publicKey, not by claimed rootId text', () => {
    // 同一担保人（同一公钥）重复担保只计一次
    expect(countDistinctVouchers([vouchOf('a'), vouchOf('b'), vouchOf('b')])).toBe(2);
    // 伪造/换手填 rootId 文本不凑数：同一公钥不同文本仍是一个担保人
    expect(countDistinctVouchers([vouchOf('a', 'req-1', 'pk-same'), vouchOf('attacker', 'req-1', 'pk-same')])).toBe(1);
    // 不同公钥即使 rootId 文本相同也按两个身份计（多设备派生口径未冻结的如实呈现）
    expect(countDistinctVouchers([vouchOf('a', 'req-1', 'pk-1'), vouchOf('a', 'req-1', 'pk-2')])).toBe(2);
  });

  it('self-vouch never counts toward the threshold', () => {
    const self = vouchOf('subject-1');
    expect(isThresholdMet([self], 1, 'subject-1')).toBe(false);
    expect(isThresholdMet([self, vouchOf('a')], 1, 'subject-1')).toBe(true);
  });

  it('builds proof payload binding proof id and sorted vouch set hash', () => {
    const vouches = [vouchOf('b'), vouchOf('a')];
    const expected = `proof:p-1:req-1:org:ctx-1:subject-1:${hashText([vouches[1].payload, vouches[0].payload].sort().join('|'))}`;
    // 担保集合顺序不影响证明载荷（排序后哈希）
    expect(buildProofPayload('p-1', 'req-1', 'org:ctx-1', 'subject-1', vouches)).toBe(expected);
    expect(buildProofPayload('p-1', 'req-1', 'org:ctx-1', 'subject-1', [...vouches].reverse())).toBe(expected);
  });

  it('assertAssemblable rejects unmet threshold and duplicate signer publicKeys', () => {
    expect(() => assertAssemblable(requestOf(2), [vouchOf('a')])).toThrow(/担保不足/);
    expect(() => assertAssemblable(requestOf(1), [vouchOf('a'), vouchOf('a')])).toThrow(/重复担保/);
    // 同一公钥换 rootId 文本仍是重复担保
    expect(() =>
      assertAssemblable(requestOf(1), [vouchOf('a', 'req-1', 'pk-same'), vouchOf('attacker', 'req-1', 'pk-same')])
    ).toThrow(/重复担保/);
    expect(() => assertAssemblable(requestOf(2), [vouchOf('a'), vouchOf('b')])).not.toThrow();
  });
});
