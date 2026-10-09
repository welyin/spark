import { describe, expect, it } from 'vitest';
import {
  ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD,
  ANNOUNCEMENT_MAX_BODY_LENGTH,
  ANNOUNCEMENT_MAX_TITLE_LENGTH,
  ANNOUNCEMENT_SUMMARY_LIMIT,
  buildAnnouncementList,
  buildAnnouncementSignPayload,
  buildAnnouncementSummary,
  buildHistorySummary,
  announcementSignContent,
  canManageAnnounceConfig,
  canPublishAnnouncement,
  canRetractAnnouncement,
  deriveRetractionMap,
  hashAnnouncementContent,
  isAnnouncementRetracted,
  retractionSignContent,
  selectBackfillBatch,
  validateAnnouncementBody,
  validateAnnouncementTitle,
  validateRetractReason,
  validateVersionFields,
  type Announcement,
  type AnnouncementConfig,
  type AnnouncementRetraction
} from '../model';

const mkAnnouncement = (overrides: Partial<Announcement> = {}): Announcement => ({
  id: 'ann-1',
  orgId: 'org-1',
  kind: 'notice',
  title: '标题',
  body: '正文',
  publisherRootId: 'root-1',
  publishedAt: 1,
  ...overrides
});

const mkRetraction = (overrides: Partial<AnnouncementRetraction> = {}): AnnouncementRetraction => ({
  id: `retract-${Math.random()}`,
  orgId: 'org-1',
  targetAnnouncementId: 'ann-1',
  retractorRootId: 'root-admin',
  retractedAt: 1,
  ...overrides
});

const mkConfig = (overrides: Partial<AnnouncementConfig> = {}): AnnouncementConfig => ({
  orgId: 'org-1',
  publisherRootIds: ['root-pub'],
  createdBy: 'root-admin',
  createdAt: 1,
  updatedAt: 1,
  ...overrides
});

describe('spark-announcement model', () => {
  // ------------------------------------------------------------------
  // 校验
  // ------------------------------------------------------------------

  it('validates title/body non-empty and length caps', () => {
    expect(validateAnnouncementTitle('  ').ok).toBe(false);
    expect(validateAnnouncementTitle('x'.repeat(ANNOUNCEMENT_MAX_TITLE_LENGTH + 1)).ok).toBe(false);
    expect(validateAnnouncementTitle('正常标题').ok).toBe(true);

    expect(validateAnnouncementBody('').ok).toBe(false);
    expect(validateAnnouncementBody('x'.repeat(ANNOUNCEMENT_MAX_BODY_LENGTH + 1)).ok).toBe(false);
    expect(validateAnnouncementBody('正常正文').ok).toBe(true);
  });

  it('validates optional version/releaseRef/reason length caps only', () => {
    expect(validateVersionFields(undefined, undefined).ok).toBe(true);
    expect(validateVersionFields('v0.2.0', 'release_1').ok).toBe(true);
    expect(validateVersionFields('v'.repeat(41), undefined).ok).toBe(false);
    expect(validateVersionFields(undefined, 'r'.repeat(121)).ok).toBe(false);

    expect(validateRetractReason(undefined).ok).toBe(true);
    expect(validateRetractReason('x'.repeat(201)).ok).toBe(false);
  });

  // ------------------------------------------------------------------
  // 权限（业务层校验；fail-closed）
  // ------------------------------------------------------------------

  it('publish right requires registered publisher set (fail-closed when config missing)', () => {
    expect(canPublishAnnouncement(mkConfig(), 'root-pub')).toBe(true);
    expect(canPublishAnnouncement(mkConfig(), 'root-other')).toBe(false);
    // 配置未初始化：任何成员都无发布路径
    expect(canPublishAnnouncement(null, 'root-pub')).toBe(false);
    expect(canPublishAnnouncement(mkConfig(), null)).toBe(false);
  });

  it('config management is roster-admin only (档三-23)', () => {
    expect(canManageAnnounceConfig('admin')).toBe(true);
    expect(canManageAnnounceConfig('member')).toBe(false);
    expect(canManageAnnounceConfig(null)).toBe(false);
  });

  it('retract right covers publisher set members and roster admins', () => {
    expect(canRetractAnnouncement(mkConfig(), 'root-pub', 'member')).toBe(true);
    expect(canRetractAnnouncement(mkConfig(), 'root-other', 'admin')).toBe(true);
    expect(canRetractAnnouncement(mkConfig(), 'root-other', 'member')).toBe(false);
    // 配置缺失时管理员仍可撤回（治理兜底）
    expect(canRetractAnnouncement(null, 'root-admin', 'admin')).toBe(true);
  });

  // ------------------------------------------------------------------
  // 签名载荷
  // ------------------------------------------------------------------

  it('hash is deterministic and stable', () => {
    expect(hashAnnouncementContent('公告正文')).toBe(hashAnnouncementContent('公告正文'));
    expect(hashAnnouncementContent('a')).not.toBe(hashAnnouncementContent('b'));
    expect(hashAnnouncementContent('')).toMatch(/^[0-9a-f]{8}$/);
  });

  it('sign payload binds org/record/publisher/content quadruple', () => {
    const content = announcementSignContent({ kind: 'release', title: 'T', body: 'B', version: 'v1', releaseRef: 'r1' });
    expect(content).toBe('release\nT\nB\nv1\nr1');
    const payload = buildAnnouncementSignPayload('org-1', 'ann-1', 'root-1', content);
    expect(payload).toBe(`org-1:ann-1:root-1:${hashAnnouncementContent(content)}`);
    // 任一字段变化 → 载荷变化
    expect(buildAnnouncementSignPayload('org-2', 'ann-1', 'root-1', content)).not.toBe(payload);
    expect(buildAnnouncementSignPayload('org-1', 'ann-2', 'root-1', content)).not.toBe(payload);
    expect(buildAnnouncementSignPayload('org-1', 'ann-1', 'root-2', content)).not.toBe(payload);
  });

  it('retraction sign content binds target and reason', () => {
    expect(retractionSignContent('ann-1', '理由')).toBe('ann-1:理由');
    expect(retractionSignContent('ann-1')).toBe('ann-1:');
  });

  // ------------------------------------------------------------------
  // summary 纪律（≤200 字符、自含完整语义）
  // ------------------------------------------------------------------

  it('builds kind-prefixed summary with version for release announcements', () => {
    expect(buildAnnouncementSummary({ kind: 'notice', title: '例会通知', body: '周三下午三点。' })).toBe(
      '【团队通知】例会通知：周三下午三点。'
    );
    expect(
      buildAnnouncementSummary({ kind: 'release', title: '新版本', body: '修复若干问题', version: 'v0.2.0' })
    ).toBe('【版本公告·v0.2.0】新版本：修复若干问题');
    // 版本公告无版本号时不拼版本段
    expect(buildAnnouncementSummary({ kind: 'release', title: '新版本', body: 'x' })).toBe('【版本公告】新版本：x');
  });

  it('trims summary to the 200-char hard cap with ellipsis (deterministic)', () => {
    const long = buildAnnouncementSummary({
      kind: 'notice',
      title: 'T'.repeat(300),
      body: 'B'.repeat(300)
    });
    expect(long.length).toBeLessThanOrEqual(ANNOUNCEMENT_SUMMARY_LIMIT);
    expect(long.endsWith('…')).toBe(true);

    const longBody = buildAnnouncementSummary({ kind: 'notice', title: '短标题', body: '正'.repeat(500) });
    expect(longBody.length).toBeLessThanOrEqual(ANNOUNCEMENT_SUMMARY_LIMIT);
    expect(longBody).toContain('【团队通知】短标题：');

    // 确定性：同输入同输出
    expect(buildAnnouncementSummary({ kind: 'notice', title: '短标题', body: '正'.repeat(500) })).toBe(longBody);
  });

  it('truncates by code points without splitting surrogate pairs (emoji-safe)', () => {
    const emojiTitle = '🎉'.repeat(150); // 300 个 UTF-16 码元，150 个码点
    const summary = buildAnnouncementSummary({ kind: 'notice', title: emojiTitle, body: '正文' });
    expect([...summary].length).toBeLessThanOrEqual(ANNOUNCEMENT_SUMMARY_LIMIT);
    // 不劈开 surrogate 对：剥省略号后不含孤立代理项
    const stripped = summary.endsWith('…') ? summary.slice(0, -1) : summary;
    expect(stripped).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/);

    // 正文截断同样码点安全
    const bodySummary = buildAnnouncementSummary({ kind: 'notice', title: '短', body: '正🎉'.repeat(200) });
    expect([...bodySummary].length).toBeLessThanOrEqual(ANNOUNCEMENT_SUMMARY_LIMIT);
    const strippedBody = bodySummary.endsWith('…') ? bodySummary.slice(0, -1) : bodySummary;
    expect(strippedBody).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/);
  });

  it('history summary is self-contained and within the cap', () => {
    const summary = buildHistorySummary(17);
    expect(summary).toContain('17');
    expect(summary).toContain('历史公告');
    expect(summary.length).toBeLessThanOrEqual(ANNOUNCEMENT_SUMMARY_LIMIT);
  });

  // ------------------------------------------------------------------
  // 撤回派生（读侧鉴权：合法撤回人集合，fail-closed）
  // ------------------------------------------------------------------

  it('derives retraction map with read-side authorization (forged retractions ignored)', () => {
    const legit = mkRetraction({ id: 'r1', retractorRootId: 'root-admin', retractedAt: 1 });
    const forged = mkRetraction({ id: 'r2', retractorRootId: 'root-attacker', retractedAt: 2 });

    const map = deriveRetractionMap([legit, forged], new Set(['root-admin']));
    // 伪造撤回（非合法撤回人）留痕但不参与派生
    expect(map.get('ann-1')?.id).toBe('r1');
    expect(isAnnouncementRetracted('ann-1', map)).toBe(true);
    expect(isAnnouncementRetracted('ann-other', map)).toBe(false);

    // 合法撤回人集合为空 = 全部忽略（fail-closed）
    expect(deriveRetractionMap([legit], new Set()).size).toBe(0);
  });

  it('keeps the latest retraction per target (deterministic tie-break by id)', () => {
    const first = mkRetraction({ id: 'r1', retractedAt: 1 });
    const second = mkRetraction({ id: 'r2', retractedAt: 2, reason: '更正' });
    const map = deriveRetractionMap([second, first], new Set(['root-admin']));
    expect(map.get('ann-1')?.id).toBe('r2');

    // retractedAt 平局按 id 字典序取较大者（后序覆盖）
    const tieA = mkRetraction({ id: 'ra', retractedAt: 5 });
    const tieB = mkRetraction({ id: 'rb', retractedAt: 5 });
    const tied = deriveRetractionMap([tieA, tieB], new Set(['root-admin']));
    expect(tied.get('ann-1')?.id).toBe('rb');
  });

  // ------------------------------------------------------------------
  // 列表组装
  // ------------------------------------------------------------------

  it('builds list sorted by publishedAt desc with retraction attached and kind filter', () => {
    const a1 = mkAnnouncement({ id: 'a1', publishedAt: 1, kind: 'notice' });
    const a2 = mkAnnouncement({ id: 'a2', publishedAt: 3, kind: 'release' });
    const a3 = mkAnnouncement({ id: 'a3', publishedAt: 2, kind: 'notice' });
    const retraction = mkRetraction({ targetAnnouncementId: 'a3' });
    const map = deriveRetractionMap([retraction], new Set(['root-admin']));

    const all = buildAnnouncementList([a1, a2, a3], map, 'all');
    expect(all.map((item) => item.announcement.id)).toEqual(['a2', 'a3', 'a1']);
    expect(all[1].retraction?.targetAnnouncementId).toBe('a3');
    expect(all[0].retraction).toBeUndefined();

    const releases = buildAnnouncementList([a1, a2, a3], map, 'release');
    expect(releases.map((item) => item.announcement.id)).toEqual(['a2']);

    // publishedAt 平局按 id 字典序（跨设备确定性）
    const t1 = mkAnnouncement({ id: 'aa', publishedAt: 9 });
    const t2 = mkAnnouncement({ id: 'ab', publishedAt: 9 });
    expect(buildAnnouncementList([t2, t1], new Map()).map((item) => item.announcement.id)).toEqual(['aa', 'ab']);
  });

  // ------------------------------------------------------------------
  // 补发节流（设计稿 §3：超阈值只补最新一条 + 汇总）
  // ------------------------------------------------------------------

  it('backfill batch sends every announcement when within threshold', () => {
    const pending = Array.from({ length: ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD }, (_, i) =>
      mkAnnouncement({ id: `a${i}`, publishedAt: i + 1 })
    );
    const batch = selectBackfillBatch(pending);
    expect(batch.cards.map((item) => item.id)).toEqual(pending.map((item) => item.id));
    expect(batch.summarizedCount).toBe(0);
    expect(batch.summarized).toEqual([]);
  });

  it('backfill batch over threshold keeps only the latest card plus a summary for the rest', () => {
    const pending = Array.from({ length: ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD + 15 }, (_, i) =>
      mkAnnouncement({ id: `a${i}`, publishedAt: i + 1 })
    );
    const batch = selectBackfillBatch(pending);
    expect(batch.cards).toHaveLength(1);
    expect(batch.cards[0].id).toBe(`a${ANNOUNCEMENT_BACKFILL_FULL_THRESHOLD + 14}`);
    expect(batch.summarizedCount).toBe(pending.length - 1);
    expect(batch.summarized).toHaveLength(pending.length - 1);
  });
});
