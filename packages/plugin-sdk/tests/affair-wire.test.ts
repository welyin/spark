import { describe, expect, it } from 'vitest';
import {
  base64Decode,
  buildGenesisDraft,
  deriveIdentity,
  normalizeObject,
  sha256Hex,
  sha256HexBytes,
  signPayload,
  validateAffairRefs,
  type AffairActor
} from '../src/affair-wire';

/**
 * 固定测试向量：Ed25519 种子 0x11*32 的公钥与身份 id（协议 §2.2 绑定：
 * identity == sha256hex(base64decode(publicKey))；向量由内核同规则算出）。
 */
const PUB_KEY = '0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=';
const IDENTITY = '10ba682c8ad13513971e8b56881aab8bd702bb807796eca81932c735a94d6e6d';
const ACTOR: AffairActor = { kind: 'person', identity: IDENTITY, publicKey: PUB_KEY };

const TARGET = 'cd'.repeat(32);

describe('affair-wire: canonical / hash / encoding（与内核 canonical.rs 同向量）', () => {
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
    expect(sha256HexBytes(new Uint8Array())).toBe(
      'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    );
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

describe('affair-wire: refs（§10 类型化暴露）', () => {
  it('validateAffairRefs accepts the §10 enum and 64-hex targets', () => {
    expect(validateAffairRefs([]).ok).toBe(true);
    expect(validateAffairRefs([{ target: TARGET, rel: 'inherit' }]).ok).toBe(true);
    expect(validateAffairRefs([{ target: TARGET, rel: 'appeal' }]).ok).toBe(true);
    expect(validateAffairRefs([{ target: TARGET, rel: 'parent' }]).ok).toBe(true);
    expect(validateAffairRefs([{ target: TARGET, rel: 'related' }]).ok).toBe(true);
  });

  it('validateAffairRefs rejects unknown rel and bad targets', () => {
    const badRel = validateAffairRefs([{ target: TARGET, rel: 'child' as never }]);
    expect(badRel.ok).toBe(false);
    expect(badRel.reason).toMatch(/未知引用关系/);
    const badTarget = validateAffairRefs([{ target: 'xyz', rel: 'inherit' }]);
    expect(badTarget.ok).toBe(false);
    expect(badTarget.reason).toMatch(/64 位小写 hex/);
  });
});

describe('affair-wire: 创世草稿构造', () => {
  it('buildGenesisDraft produces the §2.1 wire shape with refs carried verbatim', () => {
    const genesis = buildGenesisDraft(
      {
        type: 'spark-affairs:topic',
        title: '换届议题',
        summary: '继承上一届未尽事宜',
        tags: ['换届'],
        refs: [{ target: TARGET, rel: 'inherit' }],
        rules: { engine: 'b1' },
        extra: { regionCode: '110105' }
      },
      ACTOR,
      1_700_000_000_000
    ) as Record<string, unknown>;
    expect(genesis.affairV).toBe(1);
    expect(genesis.type).toBe('spark-affairs:topic');
    expect(genesis.initiator).toEqual({ kind: 'person', identity: IDENTITY, publicKey: PUB_KEY });
    // refs 真实携带（§10；自指禁令归内核，客户端只做形状校验）
    expect(genesis.refs).toEqual([{ target: TARGET, rel: 'inherit' }]);
    expect(genesis.createdAt).toBe(1_700_000_000_000);
    expect(genesis.sig).toBeUndefined();
    // extra 插件语义字段原样并入（随 affairId 被承诺）
    expect(genesis.regionCode).toBe('110105');
  });

  it('buildGenesisDraft defaults refs to empty and rejects malformed refs before signing', () => {
    const draft = buildGenesisDraft(
      { type: 't', title: 'a', summary: 'b', rules: { engine: 'b1' } },
      ACTOR,
      1
    );
    expect(draft.refs).toEqual([]);

    expect(() =>
      buildGenesisDraft(
        { type: 't', title: 'a', summary: 'b', refs: [{ target: 'bad', rel: 'inherit' }], rules: {} },
        ACTOR,
        1
      )
    ).toThrow(/64 位小写 hex/);
  });

  it('signPayload is the canonical form of the record without sig', () => {
    const draft = buildGenesisDraft({ type: 't', title: 'a', summary: 'b', rules: {} }, ACTOR, 1);
    const signed = { ...draft, sig: 'sig-1' };
    expect(signPayload(signed)).toBe(normalizeObject(draft));
    expect(signPayload(draft)).not.toContain('sig-1');
  });
});
