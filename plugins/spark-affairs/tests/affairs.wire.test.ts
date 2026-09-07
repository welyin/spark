import { describe, expect, it } from 'vitest';
import {
  base64Decode,
  buildOpDraft,
  buildRulesDoc,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload,
  type AffairActor
} from '../wire';
import type { AffairCreateInput } from '../model';

/**
 * 固定测试向量：Ed25519 种子 0x11*32 的公钥与身份 id（协议 §2.2 绑定：
 * identity == sha256hex(base64decode(publicKey))；向量由内核同规则算出）。
 *
 * 分层说明：创世草稿构造（buildGenesisDraft）已上收 SDK affair-wire
 * （sdk.affairs.create 承载），其线形断言见 packages/plugin-sdk/tests/
 * affair-wire.test.ts；本文件只测插件语义层（buildRulesDoc/buildOpDraft）
 * 与 re-export 的通用线形助手（向量防回归）。
 */
const PUB_KEY = '0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=';
const IDENTITY = '10ba682c8ad13513971e8b56881aab8bd702bb807796eca81932c735a94d6e6d';
const ACTOR: AffairActor = { kind: 'person', identity: IDENTITY, publicKey: PUB_KEY };

function validInput(overrides: Partial<AffairCreateInput> = {}): AffairCreateInput {
  return {
    title: '小区绿植补种预算',
    summary: '讨论南广场绿植补种的预算方案。',
    tags: ['预算'],
    refs: [],
    rules: {
      reviewPeriodHours: 24,
      passThreshold: 0.67,
      minQuorum: 3,
      initialVoters: [],
      entryRequirement: { kind: 'none' }
    },
    ...overrides
  };
}

describe('spark-affairs wire: canonical / hash / encoding', () => {
  it('normalizeObject matches the sync-evidence §1 JS semantics (nested values embedded as strings)', () => {
    // 与内核 core/src/evidence/canonical.rs 的验收断言逐字一致
    expect(normalizeObject({ a: { b: 1 } })).toBe('{"a":"{\\"b\\":\\"1\\"}"}');
    expect(normalizeObject({ a: 1 })).toBe('{"a":"1"}');
    expect(normalizeObject([])).toBe('{}');
    expect(normalizeObject({})).toBe('{}');
    expect(normalizeObject(null)).toBe('null');
    expect(normalizeObject(undefined)).toBe('undefined');
    expect(normalizeObject('你好')).toBe('"你好"');
    // key 排序 + 整数型 key 数值升序前置（JSON 对象固有规则）
    expect(normalizeObject({ b: 1, a: 2 })).toBe('{"a":"2","b":"1"}');
    expect(normalizeObject({ b: 1, 10: 'x', 2: 'y' })).toBe('{"2":"\\"y\\"","10":"\\"x\\"","b":"1"}');
  });

  it('sha256 matches known vectors', () => {
    expect(sha256Hex('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(sha256HexBytes(new Uint8Array())).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    // 跨块长输入（>64B 触发多分组）
    const long = new TextEncoder().encode('a'.repeat(1000));
    expect(sha256HexBytes(long)).toHaveLength(64);
  });

  it('base64Decode decodes without atob and rejects invalid input', () => {
    expect([...base64Decode('aGk=')]).toEqual([104, 105]);
    expect([...base64Decode(PUB_KEY)]).toHaveLength(32);
    expect(() => base64Decode('***')).toThrow(/base64/);
  });

  it('derives identity as sha256hex(base64decode(publicKey)) per §2.2', () => {
    expect(deriveIdentity(PUB_KEY)).toBe(IDENTITY);
  });
});

describe('spark-affairs wire: genesis / op drafts', () => {
  it('buildRulesDoc maps draft rules to the b1 rules document (plugin params under sparkAffairs)', () => {
    const doc = buildRulesDoc(validInput().rules) as any;
    expect(doc.engine).toBe('b1');
    expect(doc.pubPeriod).toEqual({ delayMs: 24 * 3600 * 1000, vetoThreshold: { count: 1 } });
    expect(doc.ruleChange.kind).toBe('delayed-veto');
    expect(doc.sparkAffairs).toEqual({ passThreshold: 0.67, minQuorum: 3 });
    expect(doc.participation).toBeUndefined();
  });

  it('buildRulesDoc maps ladder entry requirement to participation and rejects credential kind', () => {
    const withLadder = validInput();
    withLadder.rules.entryRequirement = { kind: 'ladder', minLevel: 'contributor' };
    expect((buildRulesDoc(withLadder.rules) as any).participation).toEqual({
      contribute: { ladder: 'contributor' },
      vote: { ladder: 'voter' },
      combine: 'all'
    });

    const withCredential = validInput();
    withCredential.rules.entryRequirement = { kind: 'credential', credentialType: 'hoa-owner' };
    expect(() => buildRulesDoc(withCredential.rules)).toThrow(/凭证门槛/);
  });

  it('signPayload is the canonical form of the record without sig', () => {
    // 操作记录签名载荷 = canonical(剔除 sig 全文)（community README 总约；
    // 创世侧同规则的线形断言在 SDK affair-wire 测试）
    const draft = buildOpDraft('ab'.repeat(32), ACTOR, 'comment', { text: 'x' }, [], 1);
    const signed = { ...draft, sig: 'sig-1' };
    expect(signPayload(signed)).toBe(normalizeObject(draft));
    expect(signPayload(draft)).not.toContain('sig-1');
  });

  it('buildOpDraft uses content opType, payload.kind, and causal witness (first op = affairId)', () => {
    const affairId = 'ab'.repeat(32);
    const first = buildOpDraft(affairId, ACTOR, 'contribution', { text: '议案' }, [], 1000) as any;
    expect(first.opV).toBe(1);
    expect(first.opType).toBe('content');
    expect(first.prevOpHash).toBe(affairId);
    expect(first.payload).toEqual({ kind: 'contribution', text: '议案' });
    expect(first.actor).toEqual({ kind: 'person', identity: IDENTITY, publicKey: PUB_KEY });
    expect(first.declaredAt).toBe(1000);

    // 多个 DAG 头时取字典序最小者（本地确定性选择，协议不做要求）
    const next = buildOpDraft(affairId, ACTOR, 'comment', { text: 'x' }, ['ff'.repeat(32), '11'.repeat(32)], 1001) as any;
    expect(next.prevOpHash).toBe('11'.repeat(32));
  });
});
