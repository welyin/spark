import { describe, expect, it } from 'vitest';
import {
  assertAssemblable,
  buildProofPayload,
  buildVouchPayload,
  countDistinctVouchers,
  hashText,
  isThresholdMet,
  validateRequestInput,
  type Vouch,
  type VouchRequest
} from '../model';

function vouchOf(voucherRootId: string, requestId = 'req-1'): Vouch {
  return {
    requestId,
    voucherRootId,
    payload: buildVouchPayload(requestId, 'ctx-1', 'subject-1', voucherRootId),
    signature: 'sig',
    publicKey: 'pk',
    vouchedAt: 1
  };
}

function requestOf(requiredCount = 2): VouchRequest {
  return {
    requestId: 'req-1',
    context: 'ctx-1',
    subjectRootId: 'subject-1',
    requiredCount,
    note: '',
    createdAt: 1
  };
}

describe('spark-threshold-vouch model', () => {
  it('validates request input (context/subject required, N within bounds)', () => {
    expect(validateRequestInput({ context: 'affair-x', subjectRootId: 'root-a', requiredCount: 2, note: '' }).ok).toBe(true);
    expect(validateRequestInput({ context: ' ', subjectRootId: 'root-a', requiredCount: 2, note: '' })).toEqual({
      ok: false,
      reason: '治理上下文不能为空（如事务 id）'
    });
    expect(validateRequestInput({ context: 'c', subjectRootId: '', requiredCount: 2, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'c', subjectRootId: 'r', requiredCount: 0, note: '' }).ok).toBe(false);
    expect(validateRequestInput({ context: 'c', subjectRootId: 'r', requiredCount: 1.5, note: '' }).ok).toBe(false);
  });

  it('builds vouch payload binding request/context/subject/voucher', () => {
    expect(buildVouchPayload('req-1', 'ctx-1', 'subject-1', 'voucher-1')).toBe(
      'vouch:req-1:ctx-1:subject-1:voucher-1'
    );
    expect(buildVouchPayload('req-1', 'ctx-1', 'subject-1', 'voucher-1')).not.toBe(
      buildVouchPayload('req-1', 'ctx-1', 'subject-1', 'voucher-2')
    );
  });

  it('counts distinct vouchers (one person one vote, duplicates do not stack)', () => {
    expect(countDistinctVouchers([vouchOf('a'), vouchOf('b'), vouchOf('b')])).toBe(2);
  });

  it('self-vouch never counts toward the threshold', () => {
    const self = vouchOf('subject-1');
    expect(isThresholdMet([self], 1, 'subject-1')).toBe(false);
    expect(isThresholdMet([self, vouchOf('a')], 1, 'subject-1')).toBe(true);
  });

  it('builds proof payload binding proof id and sorted vouch set hash', () => {
    const vouches = [vouchOf('b'), vouchOf('a')];
    const expected = `proof:p-1:req-1:ctx-1:subject-1:${hashText([vouches[1].payload, vouches[0].payload].sort().join('|'))}`;
    // 担保集合顺序不影响证明载荷（排序后哈希）
    expect(buildProofPayload('p-1', 'req-1', 'ctx-1', 'subject-1', vouches)).toBe(expected);
    expect(buildProofPayload('p-1', 'req-1', 'ctx-1', 'subject-1', [...vouches].reverse())).toBe(expected);
  });

  it('assertAssemblable rejects unmet threshold and duplicate vouchers', () => {
    expect(() => assertAssemblable(requestOf(2), [vouchOf('a')])).toThrow(/担保不足/);
    expect(() => assertAssemblable(requestOf(1), [vouchOf('a'), vouchOf('a')])).toThrow(/重复担保/);
    expect(() => assertAssemblable(requestOf(2), [vouchOf('a'), vouchOf('b')])).not.toThrow();
  });
});
