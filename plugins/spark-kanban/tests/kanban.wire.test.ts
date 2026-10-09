import { describe, expect, it } from 'vitest';
import { buildOpDraft, buildStatusOpPayload, deriveIdentity, signPayload } from '../wire';
import { KANBAN_STATUS_OP_KIND } from '../model';

const ACTOR = { kind: 'person', identity: 'a'.repeat(64), publicKey: 'cGsx' };

describe('spark-kanban wire · 状态操作线形（档三-1：签名操作入事务日志）', () => {
  it('builds kanban.status payload with optional prevStatus/note (trimmed)', () => {
    expect(buildStatusOpPayload({ status: ' doing ', prevStatus: 'todo', note: ' 开工 ' })).toEqual({
      kind: KANBAN_STATUS_OP_KIND,
      status: 'doing',
      prevStatus: 'todo',
      note: '开工'
    });
    // 空 prevStatus/null 与空白 note 不入载荷（canonical 承诺最小化）
    expect(buildStatusOpPayload({ status: 'todo', prevStatus: null, note: '   ' })).toEqual({
      kind: KANBAN_STATUS_OP_KIND,
      status: 'todo'
    });
  });

  it('builds op draft per affair.md §3.2 (opType=content, prevOpHash = lexicographically smallest head)', () => {
    const draft = buildOpDraft('f'.repeat(64), ACTOR, buildStatusOpPayload({ status: 'todo' }), ['zz', 'aa'], 1234);
    expect(draft).toMatchObject({
      opV: 1,
      affairId: 'f'.repeat(64),
      opType: 'content',
      prevOpHash: 'aa',
      declaredAt: 1234,
      actor: ACTOR
    });
    expect((draft.payload as Record<string, unknown>).kind).toBe('kanban.status');
    // 首条操作（无 DAG 头）：prevOpHash = affairId
    const first = buildOpDraft('f'.repeat(64), ACTOR, buildStatusOpPayload({ status: 'todo' }), [], 1);
    expect(first.prevOpHash).toBe('f'.repeat(64));
  });

  it('sign payload excludes sig field (community README 总约)', () => {
    const withSig = { a: 1, sig: 'should-be-dropped' };
    const without = { a: 1 };
    expect(signPayload(withSig)).toBe(signPayload(without));
  });

  it('deriveIdentity = sha256hex(base64decode(publicKey)) (§2.2 自包含绑定)', () => {
    // 'cGsx' base64 解码为 "pk1"；推导结果须为 64 位小写 hex
    expect(deriveIdentity('cGsx')).toMatch(/^[0-9a-f]{64}$/);
  });
});
