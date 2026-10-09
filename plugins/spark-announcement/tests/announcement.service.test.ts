import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ANNOUNCEMENT_COLLECTIONS, AnnouncementService } from '../service';
import { announcementSignContent, buildAnnouncementSignPayload, retractionSignContent } from '../model';
import type { Announcement, AnnouncementConfig, AnnouncementRetraction } from '../model';

/**
 * mock SDK：覆盖本插件用到的全部域（docs / identity / messages / data），
 * 与插件能力面一一对应——新加 SDK 调用时先在这里补 mock。
 * dataStore 可跨实例共享：模拟 iframe 销毁重建（新 service + 新 mock）后
 * 持久台账仍在（H1 验收路径）。
 */
function createMockSdk(dataStore: Map<string, unknown> = new Map()) {
  return {
    docs: {
      get: vi.fn().mockResolvedValue(null),
      put: vi.fn(),
      query: vi.fn().mockResolvedValue({ items: [], nextCursor: undefined }),
      defineCollection: vi.fn().mockResolvedValue({
        collection: 'mock',
        syncStrategy: 'append-only',
        governance: false,
        enableEvidence: true
      })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({
        domain: 'plugin:spark-announcement',
        domainId: 'spark-announcement',
        publicKey: 'pk-1',
        signature: 'sig-1',
        payloadHash: 'ph-1'
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    messages: {
      sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }),
      listAppMessages: vi.fn(),
      markRead: vi.fn()
    },
    // sdk.data 持久面（送达台账 scope:'local' 集合）：内存 Map 模拟
    data: {
      declareCollection: vi.fn().mockResolvedValue({ name: 'spark-announcement:delivery', scope: 'local' }),
      save: vi.fn(async (name: string, key: string, value: unknown) => {
        dataStore.set(`${name}${key}`, value);
        return { success: true };
      }),
      query: vi.fn(async (name: string, options?: { prefix?: string; limit?: number }) => {
        const prefix = `${name}${options?.prefix ?? ''}`;
        const items = [...dataStore.keys()]
          .filter((key) => key.startsWith(prefix))
          .map((key) => ({ key: key.slice(name.length), value: dataStore.get(key) }));
        return { items, nextCursor: undefined };
      }),
      get: vi.fn(async () => null)
    }
  } as any;
}

const mkAnnouncement = (overrides: Partial<Announcement> = {}): Announcement => ({
  id: 'ann-1',
  orgId: 'org-1',
  kind: 'notice',
  title: '标题',
  body: '正文',
  publisherRootId: 'root-pub',
  publishedAt: 1,
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

const RETRACTORS = new Set(['root-pub', 'root-admin']);

describe('spark-announcement service', () => {
  beforeEach(() => {
    // 送达台账按 orgId 存 localStorage：用例间互不相染
    globalThis.localStorage?.clear();
  });

  it('declares collection sync strategies before writing (lww config, append-only items/retractions)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);
    await service.saveConfig('org-1', 'root-admin', { publisherRootIds: ['root-pub'] }, 'admin');
    await service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: 't', body: 'b' }, mkConfig());

    const declared = sdk.docs.defineCollection.mock.calls.map((call: any[]) => [call[0], call[1]]);
    expect(declared).toEqual([
      [ANNOUNCEMENT_COLLECTIONS.config, { syncStrategy: 'lww' }],
      [ANNOUNCEMENT_COLLECTIONS.items, { syncStrategy: 'append-only' }],
      [ANNOUNCEMENT_COLLECTIONS.retractions, { syncStrategy: 'append-only' }]
    ]);
    // 声明幂等：第二次写入不再重复声明
    expect(sdk.docs.defineCollection).toHaveBeenCalledTimes(3);
  });

  // ------------------------------------------------------------------
  // 发布权配置（档三-23：MVP 名册管理员直改初始化）
  // ------------------------------------------------------------------

  it('saveConfig is roster-admin only and dedups publisher ids', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    await expect(
      service.saveConfig('org-1', 'root-member', { publisherRootIds: ['root-pub'] }, 'member')
    ).rejects.toThrow(/管理员/);

    const config = await service.saveConfig(
      'org-1',
      'root-admin',
      { publisherRootIds: ['root-pub', 'root-pub', '  root-pub2  ', ''] },
      'admin'
    );
    expect(config.publisherRootIds).toEqual(['root-pub', 'root-pub2']);
    expect(config.enableRelease).toBe(true);
    expect(config.enableNotice).toBe(true);
    expect(config.createdBy).toBe('root-admin');
    expect(sdk.docs.put.mock.calls[0][0]).toBe(ANNOUNCEMENT_COLLECTIONS.config);
    expect(sdk.docs.put.mock.calls[0][1]).toBe('org-1');
  });

  it('saveConfig preserves createdBy/createdAt on update (lww single doc)', async () => {
    const sdk = createMockSdk();
    const existing = mkConfig({ createdBy: 'root-founder', createdAt: 5 });
    sdk.docs.get.mockResolvedValue(existing);

    const service = new AnnouncementService(sdk);
    const updated = await service.saveConfig('org-1', 'root-admin', { publisherRootIds: ['root-x'] }, 'admin');
    expect(updated.createdBy).toBe('root-founder');
    expect(updated.createdAt).toBe(5);
    expect(updated.publisherRootIds).toEqual(['root-x']);
  });

  // ------------------------------------------------------------------
  // 发布（业务层发布权校验；fail-closed）
  // ------------------------------------------------------------------

  it('rejects publishing when config missing or publisher not registered (fail-closed)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    // 配置未初始化：任何成员都无发布路径
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: 't', body: 'b' }, null)
    ).rejects.toThrow(/发布权/);
    // 非发布权集合成员
    await expect(
      service.publishAnnouncement('org-1', 'root-other', { kind: 'notice', title: 't', body: 'b' }, mkConfig())
    ).rejects.toThrow(/发布权/);
    expect(sdk.docs.put).not.toHaveBeenCalled();
  });

  it('respects kind switches in config', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'release', title: 't', body: 'b' }, mkConfig({ enableRelease: false }))
    ).rejects.toThrow(/版本公告/);
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: 't', body: 'b' }, mkConfig({ enableNotice: false }))
    ).rejects.toThrow(/团队通知/);
  });

  it('publishes signed announcement; notice kind strips version/releaseRef', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const notice = await service.publishAnnouncement(
      'org-1',
      'root-pub',
      { kind: 'notice', title: '例会', body: '周三', version: 'v9', releaseRef: 'r9' },
      mkConfig()
    );
    expect(notice.version).toBeUndefined();
    expect(notice.releaseRef).toBeUndefined();

    const release = await service.publishAnnouncement(
      'org-1',
      'root-pub',
      { kind: 'release', title: '发版', body: '变更', version: ' v0.2.0 ', releaseRef: ' release_1 ' },
      mkConfig()
    );
    expect(release.version).toBe('v0.2.0');
    expect(release.releaseRef).toBe('release_1');

    // 签名载荷绑定 kind+title+body+version+releaseRef 全量
    const expectedPayload = buildAnnouncementSignPayload(
      'org-1',
      release.id,
      'root-pub',
      announcementSignContent(release)
    );
    expect(sdk.identity.sign).toHaveBeenLastCalledWith(expectedPayload);
    expect(release.signature).toEqual({ payload: expectedPayload, signature: 'sig-1', publicKey: 'pk-1' });

    const stored = sdk.docs.put.mock.calls.find((call: any[]) => call[0] === ANNOUNCEMENT_COLLECTIONS.items);
    expect(stored[1]).toBe(notice.id);
  });

  it('degrades to unsigned announcement when identity:sign is rejected (publishing not blocked)', async () => {
    const sdk = createMockSdk();
    sdk.identity.sign.mockRejectedValueOnce(new Error('Access denied: identity:sign rejected by user'));
    const service = new AnnouncementService(sdk);

    const announcement = await service.publishAnnouncement(
      'org-1',
      'root-pub',
      { kind: 'notice', title: 't', body: '拒绝也照发' },
      mkConfig()
    );
    expect(announcement.signature).toBeUndefined();
    const stored = sdk.docs.put.mock.calls.find((call: any[]) => call[0] === ANNOUNCEMENT_COLLECTIONS.items);
    expect(stored[2].signature).toBeUndefined();
  });

  it('verifies announcement signature via recomputed payload (never replays stored payload)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const announcement = await service.publishAnnouncement(
      'org-1',
      'root-pub',
      { kind: 'release', title: '标题', body: '待验签', version: 'v1' },
      mkConfig()
    );
    await expect(service.verifyAnnouncementSignature(announcement)).resolves.toBe(true);
    const expected = buildAnnouncementSignPayload('org-1', announcement.id, 'root-pub', announcementSignContent(announcement));
    expect(sdk.identity.verify).toHaveBeenCalledWith(expected, 'sig-1', 'pk-1');

    // 内容被篡改：重算载荷与随记录 payload 失配 → 直接 false，不进入密码学验签
    sdk.identity.verify.mockClear();
    await expect(service.verifyAnnouncementSignature({ ...announcement, body: '被篡改' })).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();

    // 发布者被替换同理
    await expect(service.verifyAnnouncementSignature({ ...announcement, publisherRootId: 'root-attacker' })).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();

    // 无签名直接 false
    await expect(service.verifyAnnouncementSignature({ ...announcement, signature: undefined })).resolves.toBe(false);
  });

  // ------------------------------------------------------------------
  // 撤回（append-only 撤回记录，不改原公告）
  // ------------------------------------------------------------------

  it('retracts by appending a signed retraction record; original announcement untouched', async () => {
    const sdk = createMockSdk();
    const target = mkAnnouncement({ id: 'ann-x', orgId: 'org-1' });
    sdk.docs.get.mockResolvedValue(target);
    const service = new AnnouncementService(sdk);

    const retraction = await service.retractAnnouncement(
      'org-1',
      'root-pub',
      { targetAnnouncementId: 'ann-x', reason: '内容有误' },
      mkConfig(),
      'member'
    );
    expect(retraction.targetAnnouncementId).toBe('ann-x');
    expect(retraction.reason).toBe('内容有误');
    expect(sdk.docs.put.mock.calls[0][0]).toBe(ANNOUNCEMENT_COLLECTIONS.retractions);
    // 撤回签名绑定目标 + 理由
    const expected = buildAnnouncementSignPayload('org-1', retraction.id, 'root-pub', retractionSignContent('ann-x', '内容有误'));
    expect(retraction.signature?.payload).toBe(expected);

    // 名册管理员也可撤回；普通成员不可
    await service.retractAnnouncement('org-1', 'root-admin', { targetAnnouncementId: 'ann-x' }, null, 'admin');
    await expect(
      service.retractAnnouncement('org-1', 'root-other', { targetAnnouncementId: 'ann-x' }, mkConfig(), 'member')
    ).rejects.toThrow(/撤回/);

    // 目标不存在（未同步）拦截
    sdk.docs.get.mockResolvedValueOnce(null);
    await expect(
      service.retractAnnouncement('org-1', 'root-pub', { targetAnnouncementId: 'ghost' }, mkConfig(), 'member')
    ).rejects.toThrow(/不存在/);
  });

  it('verifies retraction signature with target+reason payload', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValue(mkAnnouncement({ id: 'ann-x' }));
    const service = new AnnouncementService(sdk);

    const retraction = await service.retractAnnouncement(
      'org-1',
      'root-pub',
      { targetAnnouncementId: 'ann-x', reason: '理由' },
      mkConfig(),
      'member'
    );
    await expect(service.verifyRetractionSignature(retraction)).resolves.toBe(true);
    const expected = buildAnnouncementSignPayload('org-1', retraction.id, 'root-pub', retractionSignContent('ann-x', '理由'));
    expect(sdk.identity.verify).toHaveBeenCalledWith(expected, 'sig-1', 'pk-1');

    // 理由被篡改 → 失配
    sdk.identity.verify.mockClear();
    await expect(service.verifyRetractionSignature({ ...retraction, reason: '篡改' })).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();
  });

  // ------------------------------------------------------------------
  // message:app（服务号模型 §20.4：本地生成、本地消费）
  // ------------------------------------------------------------------

  it('sends app message with mandatory summary (≤200) and announce-card reference, then marks ledger', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const announcement = mkAnnouncement({ id: 'ann-n1', orgId: 'org-n1', kind: 'release', title: '发版', body: '变更说明', version: 'v0.2.0' });
    await expect(service.notifyAnnouncement(announcement)).resolves.toBe(true);

    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    const [payload, card] = sdk.messages.sendAppMessage.mock.calls[0];
    // 声明式摘要（强制、≤200）：未装插件时壳层原生渲染这段文本
    expect(payload.summary).toBe('【版本公告·v0.2.0】发版：变更说明');
    expect(payload.summary.length).toBeLessThanOrEqual(200);
    // 卡片只携带引用 + 定位所需 orgId：正文经 docs 查询，不随消息冗余落库
    expect(card).toEqual({ viewId: 'announce-card', data: { announcementId: 'ann-n1', orgId: 'org-n1' } });

    // 已记账：成员侧补发路径不再重复生成
    await expect(service.notifyNewAnnouncements('org-n1', [announcement], [], RETRACTORS)).resolves.toBe(0);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
  });

  it('degrades to false when app message is denied or rate-limited (publishing not blocked, rejectedCount++)', async () => {
    const sdk = createMockSdk();
    sdk.messages.sendAppMessage.mockRejectedValueOnce(new Error('rate-limited'));
    const service = new AnnouncementService(sdk);

    await expect(service.notifyAnnouncement(mkAnnouncement({ id: 'ann-rl', orgId: 'org-rl1' }))).resolves.toBe(false);
    expect(service.getDeliveryStats().rejectedCount).toBe(1);
  });

  it('returns false when messages module is absent (non-bridge context)', async () => {
    const sdk = createMockSdk();
    delete sdk.messages;
    const service = new AnnouncementService(sdk);

    await expect(service.notifyAnnouncement(mkAnnouncement())).resolves.toBe(false);
    await expect(service.notifyNewAnnouncements('org-1', [mkAnnouncement()], [], RETRACTORS)).resolves.toBe(0);
  });

  it('generates local cards only for undelivered announcements and dedups across loads (idempotent)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const a1 = mkAnnouncement({ id: 'a1', orgId: 'org-dedup', publishedAt: 1 });
    const a2 = mkAnnouncement({ id: 'a2', orgId: 'org-dedup', publishedAt: 2 });

    await expect(service.notifyNewAnnouncements('org-dedup', [a1, a2], [], RETRACTORS)).resolves.toBe(2);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(2);
    // 按发布时间升序补发
    expect(sdk.messages.sendAppMessage.mock.calls[0][1].data).toEqual({ announcementId: 'a1', orgId: 'org-dedup' });

    // 再次加载（模拟重启后）：已送达的不重复生成，仅新公告 a3 补一条
    const a3 = mkAnnouncement({ id: 'a3', orgId: 'org-dedup', publishedAt: 3 });
    await expect(service.notifyNewAnnouncements('org-dedup', [a1, a2, a3], [], RETRACTORS)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(3);
    expect(sdk.messages.sendAppMessage.mock.calls[2][1].data.announcementId).toBe('a3');
  });

  it('skips announcements already retracted at scan time (cards never generated stay silent)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const a1 = mkAnnouncement({ id: 'a1', orgId: 'org-retr', publishedAt: 1 });
    const a2 = mkAnnouncement({ id: 'a2', orgId: 'org-retr', publishedAt: 2 });
    const retraction: AnnouncementRetraction = {
      id: 'r1',
      orgId: 'org-retr',
      targetAnnouncementId: 'a1',
      retractorRootId: 'root-admin',
      retractedAt: 3
    };

    // a1 已被合法撤回人撤回：不生成卡片；a2 正常生成
    await expect(service.notifyNewAnnouncements('org-retr', [a1, a2], [retraction], RETRACTORS)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    expect(sdk.messages.sendAppMessage.mock.calls[0][1].data.announcementId).toBe('a2');

    // 伪造撤回（非合法撤回人）不参与派生：a1 仍会被补发
    const forged = { ...retraction, retractorRootId: 'root-attacker' };
    await expect(service.notifyNewAnnouncements('org-retr-2', [{ ...a1, orgId: 'org-retr-2' }], [{ ...forged, orgId: 'org-retr-2' }], RETRACTORS)).resolves.toBe(1);
  });

  it('throttles large backlog: latest card + one history summary, the rest marked delivered (防刷屏)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const backlog = Array.from({ length: 20 }, (_, i) =>
      mkAnnouncement({ id: `h${String(i).padStart(2, '0')}`, orgId: 'org-backlog', publishedAt: i + 1 })
    );

    const sent = await service.notifyNewAnnouncements('org-backlog', backlog, [], RETRACTORS);
    // 只逐条补最新一条
    expect(sent).toBe(1);
    // 总两条消息：汇总 + 最新卡片（远低于内核 10 条/60s 配额）
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(2);
    const [summaryPayload, summaryCard] = sdk.messages.sendAppMessage.mock.calls[0];
    expect(summaryPayload.summary).toContain('另有 19 条历史公告');
    expect(summaryCard).toBeUndefined();
    expect(sdk.messages.sendAppMessage.mock.calls[1][1].data.announcementId).toBe('h19');

    // 重启后不重发：全部已记账
    await expect(service.notifyNewAnnouncements('org-backlog', backlog, [], RETRACTORS)).resolves.toBe(0);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(2);
  });

  it('stops local generation batch on send failure and retries unmarked announcements later (no retry storm)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);

    const a1 = mkAnnouncement({ id: 'a1', orgId: 'org-rl2', publishedAt: 1 });
    const a2 = mkAnnouncement({ id: 'a2', orgId: 'org-rl2', publishedAt: 2 });

    // 第二条触发限流：本轮中止，a1 已记账、a2 未记账
    sdk.messages.sendAppMessage.mockResolvedValueOnce({ id: 'm1' }).mockRejectedValueOnce(new Error('rate-limited'));
    await expect(service.notifyNewAnnouncements('org-rl2', [a1, a2], [], RETRACTORS)).resolves.toBe(1);
    expect(service.getDeliveryStats().rejectedCount).toBe(1);

    // 下次加载时只补发未记账的 a2
    await expect(service.notifyNewAnnouncements('org-rl2', [a1, a2], [], RETRACTORS)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(3);
    expect(sdk.messages.sendAppMessage.mock.calls[2][0].announcementId).toBe('a2');
  });

  it('aborts the whole backfill round when the history summary send fails (retry next load)', async () => {
    const sdk = createMockSdk();
    sdk.messages.sendAppMessage.mockRejectedValueOnce(new Error('rate-limited'));
    const service = new AnnouncementService(sdk);

    const backlog = Array.from({ length: 8 }, (_, i) =>
      mkAnnouncement({ id: `s${i}`, orgId: 'org-sfail', publishedAt: i + 1 })
    );
    // 汇总消息即被限流：本轮整体中止，什么都不记账
    await expect(service.notifyNewAnnouncements('org-sfail', backlog, [], RETRACTORS)).resolves.toBe(0);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);

    // 下次加载重新走完整批次
    await expect(service.notifyNewAnnouncements('org-sfail', backlog, [], RETRACTORS)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(3);
  });

  // ------------------------------------------------------------------
  // 送达台账持久面（H1：opaque origin 沙箱 localStorage 恒抛，台账以 sdk.data 为准）
  // ------------------------------------------------------------------

  it('persists delivery ledger in sdk.data local collection (survives iframe rebuild)', async () => {
    const store = new Map<string, unknown>();
    const sdk1 = createMockSdk(store);
    const service1 = new AnnouncementService(sdk1);

    const a1 = mkAnnouncement({ id: 'p1', orgId: 'org-persist', publishedAt: 1 });
    await expect(service1.notifyNewAnnouncements('org-persist', [a1], [], RETRACTORS)).resolves.toBe(1);
    expect(sdk1.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    // 台账声明为 scope:'local' 集合并写入持久面（键 {orgId}:{announcementId}，值带 publishedAt 水位）
    expect(sdk1.data.declareCollection).toHaveBeenCalledWith({ name: 'spark-announcement:delivery', scope: 'local' });
    expect(sdk1.data.save).toHaveBeenCalledWith('spark-announcement:delivery', 'org-persist:p1', { publishedAt: 1 });

    // iframe 销毁重建：全新 service 实例 + 全新 messages mock，共享同一 data 存储——不重发
    const sdk2 = createMockSdk(store);
    const service2 = new AnnouncementService(sdk2);
    await expect(service2.notifyNewAnnouncements('org-persist', [a1], [], RETRACTORS)).resolves.toBe(0);
    expect(sdk2.messages.sendAppMessage).not.toHaveBeenCalled();
  });

  it('dedups via persistent ledger even when localStorage is unavailable (opaque origin sandbox)', async () => {
    // 模拟 opaque origin iframe：访问 localStorage 恒抛 SecurityError
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get() {
        throw new Error('SecurityError: opaque origin');
      }
    });
    try {
      const store = new Map<string, unknown>();
      const sdk = createMockSdk(store);
      const service = new AnnouncementService(sdk);

      const a1 = mkAnnouncement({ id: 's1', orgId: 'org-nols', publishedAt: 1 });
      await expect(service.notifyNewAnnouncements('org-nols', [a1], [], RETRACTORS)).resolves.toBe(1);
      expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);

      // 再次加载（localStorage 仍不可用）：持久台账去重，不重发
      await expect(service.notifyNewAnnouncements('org-nols', [a1], [], RETRACTORS)).resolves.toBe(0);
      expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    } finally {
      if (descriptor) {
        Object.defineProperty(globalThis, 'localStorage', descriptor);
      }
    }
  });

  it('falls back to localStorage cache when the data plane fails (session-scoped dedup)', async () => {
    const sdk = createMockSdk();
    sdk.data.query.mockRejectedValue(new Error('data plane down'));
    sdk.data.save.mockRejectedValue(new Error('data plane down'));
    const service = new AnnouncementService(sdk);

    const a1 = mkAnnouncement({ id: 'c1', orgId: 'org-cache', publishedAt: 1 });
    await expect(service.notifyNewAnnouncements('org-cache', [a1], [], RETRACTORS)).resolves.toBe(1);
    // 缓存记账生效：同实例再次加载不重发（读侧降级到缓存）
    await expect(service.notifyNewAnnouncements('org-cache', [a1], [], RETRACTORS)).resolves.toBe(0);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
  });

  // ------------------------------------------------------------------
  // 并发串行化（S3：同空间送达操作进程内 mutex）
  // ------------------------------------------------------------------

  it('serializes concurrent backfill batches per org (no duplicate cards)', async () => {
    const sdk = createMockSdk();
    // 人为延迟放大竞态窗口
    sdk.messages.sendAppMessage.mockImplementation(
      () => new Promise((resolve) => setTimeout(() => resolve({ id: 'm' }), 5))
    );
    const service = new AnnouncementService(sdk);

    const a1 = mkAnnouncement({ id: 'm1', orgId: 'org-mutex', publishedAt: 1 });
    const a2 = mkAnnouncement({ id: 'm2', orgId: 'org-mutex', publishedAt: 2 });
    const [r1, r2] = await Promise.all([
      service.notifyNewAnnouncements('org-mutex', [a1, a2], [], RETRACTORS),
      service.notifyNewAnnouncements('org-mutex', [a1, a2], [], RETRACTORS)
    ]);
    // 两批并发合流后总发送 = 去重后的公告数，无重复卡片
    expect(r1 + r2).toBe(2);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(2);
  });

  // ------------------------------------------------------------------
  // 服务层入口校验（U2：不能只靠视图层把关，fail-closed）
  // ------------------------------------------------------------------

  it('rejects oversized title/body/version fields at the service layer (fail-closed)', async () => {
    const sdk = createMockSdk();
    const service = new AnnouncementService(sdk);
    const config = mkConfig();

    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: 'x'.repeat(121), body: 'b' }, config)
    ).rejects.toThrow(/标题/);
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: '  ', body: 'b' }, config)
    ).rejects.toThrow(/标题/);
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'notice', title: 't', body: '' }, config)
    ).rejects.toThrow(/正文/);
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'release', title: 't', body: 'b', version: 'v'.repeat(41) }, config)
    ).rejects.toThrow(/版本号/);
    await expect(
      service.publishAnnouncement('org-1', 'root-pub', { kind: 'release', title: 't', body: 'b', releaseRef: 'r'.repeat(121) }, config)
    ).rejects.toThrow(/引用/);
    expect(sdk.docs.put).not.toHaveBeenCalled();
  });

  it('rejects oversized retract reason at the service layer', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValue(mkAnnouncement({ id: 'ann-x' }));
    const service = new AnnouncementService(sdk);

    await expect(
      service.retractAnnouncement('org-1', 'root-pub', { targetAnnouncementId: 'ann-x', reason: 'x'.repeat(201) }, mkConfig(), 'member')
    ).rejects.toThrow(/撤回理由/);
    expect(sdk.docs.put).not.toHaveBeenCalled();
  });

  // ------------------------------------------------------------------
  // 查询（orgId 一级过滤，跨设备同步口径稳定）
  // ------------------------------------------------------------------

  it('queries announcements/retractions by orgId and sorts deterministically', async () => {
    const sdk = createMockSdk();
    const items = [
      mkAnnouncement({ id: 'a1', publishedAt: 2 }),
      mkAnnouncement({ id: 'a2', publishedAt: 5 })
    ];
    sdk.docs.query.mockResolvedValue({ items: items.map((data) => ({ data })), nextCursor: undefined });

    const service = new AnnouncementService(sdk);
    const loaded = await service.loadAnnouncements('org-xyz');
    expect(loaded.map((item) => item.id)).toEqual(['a2', 'a1']);
    expect(sdk.docs.query.mock.calls[0][0]).toBe(ANNOUNCEMENT_COLLECTIONS.items);
    expect(sdk.docs.query.mock.calls[0][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });

    await service.loadRetractions('org-xyz');
    expect(sdk.docs.query.mock.calls[1][0]).toBe(ANNOUNCEMENT_COLLECTIONS.retractions);
    expect(sdk.docs.query.mock.calls[1][1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
  });
});
