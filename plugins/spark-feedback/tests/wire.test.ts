import { describe, expect, it } from 'vitest';
import {
  buildExportDraft,
  buildFeedbackGenesisInput,
  buildGenesisDraft,
  buildParentRef,
  defaultFeedbackRules,
  signPayload,
  validateAffairRefs
} from '../wire';
import { FEEDBACK_CHANNEL, type FeedbackPayload } from '../model';

const PROJECT_ID = 'ab'.repeat(32);

const payload: FeedbackPayload = {
  body: '升级到 0.3.2 后首次启动白屏。',
  reproduction: '1. 安装\n2. 启动',
  environment: { spaceKind: 'personal', appVersion: '0.3.2', platform: 'windows' },
  attachments: [{ cid: 'c'.repeat(64), name: 'shot.png', size: 128, mime: 'image/png' }],
  feedbackChannel: FEEDBACK_CHANNEL
};

describe('buildParentRef / validateAffairRefs（affair.md §10）', () => {
  it('parent 引用线形；形状校验', () => {
    const ref = buildParentRef(PROJECT_ID);
    expect(ref).toEqual({ target: PROJECT_ID, rel: 'parent' });
    expect(validateAffairRefs([ref]).ok).toBe(true);
    expect(validateAffairRefs([{ target: 'x', rel: 'parent' }]).ok).toBe(false);
    expect(validateAffairRefs([{ target: PROJECT_ID, rel: 'child' as never }]).ok).toBe(false);
  });
});

describe('buildFeedbackGenesisInput（档一-1 主路径创世输入）', () => {
  it('type=bug|proposal、refs parent、载荷入 extra.feedback', () => {
    const input = buildFeedbackGenesisInput({
      type: 'bug',
      title: ' 启动后白屏 ',
      targetAffairId: PROJECT_ID,
      payload
    });
    expect(input.type).toBe('bug');
    expect(input.title).toBe('启动后白屏'); // 标题 trim（buildGenesisDraft 同口径）
    expect(input.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    expect(input.tags).toEqual(['feedback', 'bug']);
    expect((input.extra as { feedback: FeedbackPayload }).feedback).toEqual(payload);
    // 缺省不携带 publish（fail-closed：显式声明才公开，档二-2 补录）
    expect('publish' in input).toBe(false);
  });

  it('summary 取 body 前 200 字符；proposal 类型同形', () => {
    const input = buildFeedbackGenesisInput({
      type: 'proposal',
      title: '增加深色模式',
      targetAffairId: PROJECT_ID,
      payload: { body: 'x'.repeat(300), feedbackChannel: FEEDBACK_CHANNEL }
    });
    expect(input.type).toBe('proposal');
    expect(input.summary).toHaveLength(200);
  });

  it('规则文档为容器合法性最小声明（b1 + delayed-veto）', () => {
    const rules = defaultFeedbackRules();
    expect(rules.engine).toBe('b1');
    expect(rules.ruleChange).toMatchObject({ kind: 'delayed-veto' });
  });

  it('创世草稿构造（SDK 线形）：refs 形状非法在签名前拒绝', () => {
    const input = buildFeedbackGenesisInput({
      type: 'bug',
      title: 't',
      targetAffairId: PROJECT_ID,
      payload
    });
    const draft = buildGenesisDraft(input, { kind: 'person', identity: 'id', publicKey: 'pk' }, 1700000000000);
    expect(draft.affairV).toBe(1);
    expect(draft.type).toBe('bug');
    expect(draft.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    expect((draft as Record<string, unknown>).feedback).toEqual(payload);
    // 签名载荷 = canonical(记录剔除 sig)
    expect(typeof signPayload(draft)).toBe('string');

    const bad = { ...input, refs: [{ target: 'bad', rel: 'parent' as const }] };
    expect(() => buildGenesisDraft(bad, { kind: 'person', identity: 'id', publicKey: 'pk' }, 0)).toThrow();
  });
});

describe('buildExportDraft（手动兜底导出）', () => {
  it('导出 JSON 含 schema 标识、操作指引与完整创世输入', () => {
    const genesisInput = buildFeedbackGenesisInput({
      type: 'bug',
      title: '启动后白屏',
      targetAffairId: PROJECT_ID,
      payload
    });
    const text = buildExportDraft(genesisInput);
    const parsed = JSON.parse(text);
    expect(parsed.$schema).toBe('spark-feedback/export-draft@1');
    expect(parsed.hint).toContain('项目');
    expect(parsed.genesisInput.type).toBe('bug');
    expect(parsed.genesisInput.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    expect(parsed.genesisInput.extra.feedback.feedbackChannel).toBe(FEEDBACK_CHANNEL);
  });
});
