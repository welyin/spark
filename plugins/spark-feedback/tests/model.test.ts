import { describe, expect, it } from 'vitest';
import {
  DAILY_GENTLE_LIMIT,
  FEEDBACK_CHANNEL,
  buildEnvironment,
  buildFeedbackPayload,
  countTodaySubmissions,
  dailyLimitNotice,
  hasBridgeEnvironment,
  isValidAffairId,
  localDayRange,
  parseDraft,
  parseLedgerEntry,
  parsePrefs,
  statusFromResolutionStates,
  validateFeedbackInput,
  type FeedbackInput
} from '../model';

const AFFAIR_ID = 'ab'.repeat(32);

function validInput(overrides: Partial<FeedbackInput> = {}): FeedbackInput {
  return {
    type: 'bug',
    title: '启动后白屏',
    body: '升级到 0.3.2 后首次启动白屏，重启无效。',
    reproduction: '1. 安装 0.3.2\n2. 启动',
    includeEnvironment: true,
    attachments: [],
    ...overrides
  };
}

describe('isValidAffairId', () => {
  it('64 位小写 hex 通过；其他形状拒绝', () => {
    expect(isValidAffairId(AFFAIR_ID)).toBe(true);
    expect(isValidAffairId(AFFAIR_ID.toUpperCase())).toBe(false);
    expect(isValidAffairId('ab'.repeat(31))).toBe(false);
    expect(isValidAffairId(null)).toBe(false);
    expect(isValidAffairId(42)).toBe(false);
  });
});

describe('validateFeedbackInput', () => {
  it('合法输入通过（bug 含复现步骤 / proposal 无复现步骤）', () => {
    expect(validateFeedbackInput(validInput()).ok).toBe(true);
    expect(validateFeedbackInput(validInput({ type: 'proposal', reproduction: undefined })).ok).toBe(true);
  });

  it('类型 / 标题 / 描述边界逐条拦截', () => {
    expect(validateFeedbackInput(validInput({ type: 'other' as never })).ok).toBe(false);
    expect(validateFeedbackInput(validInput({ title: '短' })).ok).toBe(false);
    expect(validateFeedbackInput(validInput({ title: 'x'.repeat(81) })).ok).toBe(false);
    expect(validateFeedbackInput(validInput({ body: '太短' })).ok).toBe(false);
    expect(validateFeedbackInput(validInput({ body: 'x'.repeat(4001) })).ok).toBe(false);
  });

  it('附件 cid 形状非法即拒（cid 校验一致纪律）', () => {
    const bad = { cid: 'not-hex', name: 'a.png', size: 10 };
    const verdict = validateFeedbackInput(validInput({ attachments: [bad] }));
    expect(verdict.ok).toBe(false);
    expect(verdict.reason).toContain('cid');
  });
});

describe('buildEnvironment（档二-9：ctx 注入缺什么省什么）', () => {
  it('全字段注入时全部携带；spaceKind 恒有值', () => {
    const env = buildEnvironment(
      { space: { type: 'personal', id: 'personal' }, appVersion: '0.3.2', platform: 'windows', shellVersion: '1' },
      undefined
    );
    expect(env).toEqual({ spaceKind: 'personal', appVersion: '0.3.2', platform: 'windows', shellVersion: '1' });
  });

  it('旧壳层缺省字段 → 省略不编造；手填版本号走 reportedVersion', () => {
    const env = buildEnvironment({ space: { type: 'org', id: 'org-1' } }, ' 0.3.2 ');
    expect(env).toEqual({ spaceKind: 'org', reportedVersion: '0.3.2' });
    expect('appVersion' in env).toBe(false);
  });

  it('hasBridgeEnvironment 判定降级入口', () => {
    expect(hasBridgeEnvironment({ appVersion: '0.3.2' })).toBe(true);
    expect(hasBridgeEnvironment({})).toBe(false);
  });
});

describe('buildFeedbackPayload（§3.1 载荷形状）', () => {
  it('含 body/feedbackChannel；opt-in 关闭时无 environment；空复现不携带', () => {
    const env = buildEnvironment({ space: { type: 'personal', id: 'personal' }, appVersion: '0.3.2' });
    const payload = buildFeedbackPayload(validInput({ reproduction: '  ' }), env);
    expect(payload.body).toBe('升级到 0.3.2 后首次启动白屏，重启无效。');
    expect(payload.feedbackChannel).toBe(FEEDBACK_CHANNEL);
    expect(payload.environment).toEqual(env);
    expect('reproduction' in payload).toBe(false);
    expect('attachments' in payload).toBe(false);
  });

  it('includeEnvironment=false → 无 environment 段；附件线形含 cid/name/size/mime', () => {
    const env = buildEnvironment({ space: { type: 'personal', id: 'personal' } });
    const payload = buildFeedbackPayload(
      validInput({
        includeEnvironment: false,
        attachments: [{ cid: 'c'.repeat(64), name: 'shot.png', size: 128, mime: 'image/png' }]
      }),
      env
    );
    expect('environment' in payload).toBe(false);
    expect(payload.attachments).toEqual([{ cid: 'c'.repeat(64), name: 'shot.png', size: 128, mime: 'image/png' }]);
  });
});

describe('每日提交数温馨提示（档三-18）', () => {
  it('localDayRange 为本地日历日 [start, end)', () => {
    const now = new Date(2026, 9, 10, 15, 30).getTime();
    const { start, end } = localDayRange(now);
    expect(new Date(start).getHours()).toBe(0);
    expect(end - start).toBe(86_400_000);
    expect(now).toBeGreaterThanOrEqual(start);
    expect(now).toBeLessThan(end);
  });

  it('countTodaySubmissions 只计今日', () => {
    const now = new Date(2026, 9, 10, 15, 30).getTime();
    const { start } = localDayRange(now);
    const entries = [
      { submittedAt: start + 1000 },
      { submittedAt: start - 1 }, // 昨天
      { submittedAt: start + 86_400_000 } // 明天
    ];
    expect(countTodaySubmissions(entries, now)).toBe(1);
  });

  it('未达阈值无提示；达阈值给提示文案（不阻断）', () => {
    expect(dailyLimitNotice(DAILY_GENTLE_LIMIT - 1)).toBeNull();
    const notice = dailyLimitNotice(DAILY_GENTLE_LIMIT + 2);
    expect(notice).toContain(`${DAILY_GENTLE_LIMIT + 2}`);
    expect(notice).toContain('规则文档');
  });
});

describe('读侧解析（同步面数据防御性）', () => {
  const entry = {
    id: 'fb-1',
    targetAffairId: AFFAIR_ID,
    childAffairId: 'cd'.repeat(32),
    type: 'bug',
    title: '白屏',
    submittedAt: 1700000000000
  };

  it('parseLedgerEntry：合法通过；缺字段/坏形状拒绝', () => {
    expect(parseLedgerEntry(entry)).toEqual(entry);
    expect(parseLedgerEntry({ ...entry, targetAffairId: 'x' })).toBeNull();
    expect(parseLedgerEntry({ ...entry, type: 'other' })).toBeNull();
    expect(parseLedgerEntry(null)).toBeNull();
  });

  it('parseDraft：附件过滤坏形状项；targetAffairId 非法归空串', () => {
    const draft = parseDraft('d1', {
      type: 'proposal',
      title: 't',
      body: 'b',
      reproduction: '',
      attachments: [{ cid: 'c'.repeat(64), name: 'a', size: 1 }, { cid: 'bad', name: 'b', size: 2 }],
      targetAffairId: 'bad',
      savedAt: 1
    });
    expect(draft?.attachments).toHaveLength(1);
    expect(draft?.targetAffairId).toBe('');
    expect(parseDraft('d2', { type: 'nope' })).toBeNull();
  });

  it('parsePrefs：非法 defaultTargetAffairId 不携带', () => {
    expect(parsePrefs({ defaultTargetAffairId: AFFAIR_ID, includeEnvironment: false })).toEqual({
      defaultTargetAffairId: AFFAIR_ID,
      includeEnvironment: false
    });
    expect(parsePrefs({ defaultTargetAffairId: 'bad' })).toEqual({});
    expect(parsePrefs(null)).toEqual({});
  });
});

describe('statusFromResolutionStates（本地副本所见，如实呈现）', () => {
  it('effective 优先；pending/unanchored 次之；全 vetoed 如实标注；空 = none', () => {
    expect(statusFromResolutionStates(['effective', 'vetoed'])).toBe('effective');
    expect(statusFromResolutionStates(['pending'])).toBe('pending');
    expect(statusFromResolutionStates(['unanchored'])).toBe('pending');
    expect(statusFromResolutionStates(['vetoed'])).toBe('vetoed');
    expect(statusFromResolutionStates([])).toBe('none');
  });
});
