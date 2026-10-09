import { describe, expect, it, vi } from 'vitest';
import { ForumService, FORUM_COLLECTIONS } from '../service';
import { buildForumSignPayload } from '../model';
import type { ForumBoard, ForumTopic } from '../model';

/**
 * mock SDK：覆盖本插件用到的全部域（docs / identity / messages），
 * 与插件能力面一一对应——新加 SDK 调用时先在这里补 mock。
 */
function createMockSdk() {
  return {
    docs: {
      // 默认返回一个未归档板块（createTopic 的板块存在性/归档校验用）；
      // 需要「不存在/已归档」语义的用例自行 mockResolvedValue(Once) 覆盖
      get: vi.fn().mockResolvedValue({
        id: 'board-1',
        orgId: 'org-1',
        name: '内核',
        intro: '',
        sort: 0,
        archived: false,
        createdBy: 'root-admin',
        createdAt: 1,
        updatedAt: 1
      }),
      put: vi.fn(),
      // 默认空查询结果（createReply 的关闭状态派生用 loadTopicEvents）
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
        domain: 'plugin:spark-forum',
        domainId: 'spark-forum',
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
    }
  } as any;
}

const mkTopic = (overrides: Partial<ForumTopic> = {}): ForumTopic => ({
  id: 'topic-1',
  orgId: 'org-1',
  boardId: 'board-1',
  title: '标题',
  content: '正文',
  contentFormat: 'plain',
  authorRootId: 'root-1',
  createdAt: 1,
  ...overrides
});

describe('spark-forum service', () => {
  it('declares collection sync strategies before writing (lww config/boards, append-only content/events)', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValueOnce(null);

    const service = new ForumService(sdk);
    await service.ensureOrgConfig('org-1', 'root-admin');
    await service.createTopic(
      'org-1',
      'root-member',
      { boardId: 'board-1', title: '标题', content: '正文' },
      'member'
    );

    const declared = sdk.docs.defineCollection.mock.calls.map((call: any[]) => [call[0], call[1]]);
    expect(declared).toEqual([
      [FORUM_COLLECTIONS.orgConfig, { syncStrategy: 'lww' }],
      [FORUM_COLLECTIONS.boards, { syncStrategy: 'lww' }],
      [FORUM_COLLECTIONS.topics, { syncStrategy: 'append-only' }],
      [FORUM_COLLECTIONS.replies, { syncStrategy: 'append-only' }],
      // 档三-10：事件流 append-only 即可，不声明 governance:true
      [FORUM_COLLECTIONS.topicEvents, { syncStrategy: 'append-only' }]
    ]);
    // 声明幂等：第二次写入不再重复声明
    expect(sdk.docs.defineCollection).toHaveBeenCalledTimes(5);
  });

  it('sets creator as super admin on first org config (boardsSeeded flag initialized)', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValueOnce(null);

    const service = new ForumService(sdk);
    const config = await service.ensureOrgConfig('org-1', 'root-admin');

    expect(config.orgId).toBe('org-1');
    expect(config.superAdminRootId).toBe('root-admin');
    expect(config.boardsSeeded).toBe(false);
    expect(sdk.docs.put).toHaveBeenCalledTimes(1);
    expect(sdk.docs.put.mock.calls[0][0]).toBe(FORUM_COLLECTIONS.orgConfig);
  });

  it('seeds a default board with deterministic id (idempotent across devices)', async () => {
    const sdk = createMockSdk();
    sdk.docs.get.mockResolvedValue(null);
    const service = new ForumService(sdk);

    const board = await service.seedDefaultBoard('org-1', 'root-admin');
    expect(board.id).toBe('board_default_org-1');
    expect(board.name).toBe('综合讨论');

    // 已存在时不重复写入（幂等）
    sdk.docs.get.mockResolvedValueOnce(board);
    const again = await service.seedDefaultBoard('org-1', 'root-admin');
    expect(again.id).toBe(board.id);
    expect(sdk.docs.put).toHaveBeenCalledTimes(1);
  });

  it('marks boardsSeeded on org config after seeding (lww single doc)', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const updated = await service.markBoardsSeeded({
      orgId: 'org-1',
      superAdminRootId: 'root-admin',
      createdBy: 'root-admin',
      createdAt: 1,
      boardsSeeded: false
    });
    expect(updated.boardsSeeded).toBe(true);
    expect(sdk.docs.put.mock.calls[0][0]).toBe(FORUM_COLLECTIONS.orgConfig);
    expect(sdk.docs.put.mock.calls[0][1]).toBe('org-1');
  });

  // ------------------------------------------------------------------
  // 板块管理（档三-11：仅名册管理员）
  // ------------------------------------------------------------------

  it('allows admin to create/update/archive boards but rejects members', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const board = await service.createBoard('org-1', 'root-admin', { name: '内核', intro: '内核讨论' }, 'admin');
    expect(board.orgId).toBe('org-1');
    expect(board.archived).toBe(false);

    await expect(service.createBoard('org-1', 'root-member', { name: 'x', intro: '' }, 'member')).rejects.toThrow(
      /管理员/
    );
    await expect(service.updateBoard(board, { name: '改名' }, 'member')).rejects.toThrow(/管理员/);
    await expect(service.archiveBoard(board, true, 'member')).rejects.toThrow(/管理员/);

    const updated = await service.updateBoard(board, { name: '内核与运行时' }, 'admin');
    expect(updated.name).toBe('内核与运行时');
    const archived = await service.archiveBoard(board, true, 'admin');
    expect(archived.archived).toBe(true);
  });

  // ------------------------------------------------------------------
  // 主题（全员可发；编辑=新版本）
  // ------------------------------------------------------------------

  it('allows members to create topics (permission inversion) and rejects non-members', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const topic = await service.createTopic('org-1', 'root-member', { boardId: 'b1', title: '标题', content: '正文' }, 'member');
    expect(topic.authorRootId).toBe('root-member');
    expect(topic.contentFormat).toBe('plain');
    expect(topic.supersedesId).toBeUndefined();

    await expect(
      service.createTopic('org-1', 'root-outsider', { boardId: 'b1', title: 't', content: 'c' }, null)
    ).rejects.toThrow(/成员/);
  });

  it('edit-as-new-version: supersedes points to old version, author-only', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const v1 = await service.createTopic('org-1', 'root-author', { boardId: 'b1', title: '初版', content: '一' }, 'member');
    const v2 = await service.createTopic(
      'org-1',
      'root-author',
      { boardId: 'b1', title: '修订', content: '二', supersedes: v1 },
      'member'
    );
    expect(v2.supersedesId).toBe(v1.id);
    expect(v2.id).not.toBe(v1.id);

    // 非作者不能对别人的主题发新版本
    await expect(
      service.createTopic('org-1', 'root-other', { boardId: 'b1', title: 't', content: 'c', supersedes: v1 }, 'member')
    ).rejects.toThrow(/自己/);
  });

  it('signs topic with payload bound to title + content, stored on the record', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const topic = await service.createTopic('org-1', 'root-1', { boardId: 'b1', title: '标题', content: '正文' }, 'member');
    const expectedPayload = buildForumSignPayload('org-1', topic.id, 'root-1', '标题\n正文');
    expect(sdk.identity.sign).toHaveBeenCalledWith(expectedPayload);
    expect(topic.signature).toEqual({ payload: expectedPayload, signature: 'sig-1', publicKey: 'pk-1' });
  });

  it('degrades to unsigned topic when identity:sign is rejected (posting not blocked)', async () => {
    const sdk = createMockSdk();
    sdk.identity.sign.mockRejectedValueOnce(new Error('Access denied: identity:sign rejected by user'));
    const service = new ForumService(sdk);

    const topic = await service.createTopic('org-1', 'root-1', { boardId: 'b1', title: 't', content: '拒绝也照发' }, 'member');
    expect(topic.signature).toBeUndefined();
    const stored = sdk.docs.put.mock.calls.find((call: any[]) => call[0] === FORUM_COLLECTIONS.topics);
    expect(stored[2].signature).toBeUndefined();
  });

  it('verifies topic signature via recomputed payload (never replays stored payload)', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const topic = await service.createTopic('org-1', 'root-1', { boardId: 'b1', title: '标题', content: '待验签' }, 'member');
    await expect(service.verifyTopicSignature(topic)).resolves.toBe(true);
    const expected = buildForumSignPayload('org-1', topic.id, 'root-1', '标题\n待验签');
    expect(sdk.identity.verify).toHaveBeenCalledWith(expected, 'sig-1', 'pk-1');

    // 内容被篡改：重算载荷与随帖 payload 失配 → 直接 false，不进入密码学验签
    sdk.identity.verify.mockClear();
    const tampered = { ...topic, content: '被篡改' };
    await expect(service.verifyTopicSignature(tampered)).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();

    // 作者被替换同理
    const swapped = { ...topic, authorRootId: 'root-attacker' };
    await expect(service.verifyTopicSignature(swapped)).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();

    // 无签名直接 false
    await expect(service.verifyTopicSignature({ ...topic, signature: undefined })).resolves.toBe(false);
  });

  // ------------------------------------------------------------------
  // 回复（两级楼中楼，签名可选）
  // ------------------------------------------------------------------

  it('creates replies with parent relation and signature', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);
    const admins = new Set(['root-admin']);

    const reply = await service.createReply('org-1', 'root-1', { topicId: 't1', content: '一楼' }, 'member', admins);
    const child = await service.createReply(
      'org-1',
      'root-2',
      { topicId: 't1', content: '二楼', parentReplyId: reply.id },
      'member',
      admins
    );

    expect(reply.parentReplyId).toBeUndefined();
    expect(child.parentReplyId).toBe(reply.id);
    expect(sdk.docs.put).toHaveBeenCalledTimes(2);
    // 回复也走签名（回复可签名）
    expect(sdk.identity.sign).toHaveBeenCalledWith(buildForumSignPayload('org-1', reply.id, 'root-1', '一楼'));
    expect(child.signature).toBeDefined();

    await expect(
      service.createReply('org-1', 'root-outsider', { topicId: 't1', content: 'x' }, null, admins)
    ).rejects.toThrow(/成员/);
  });

  it('rejects replies to topics closed by a roster admin (derived state, read-side authz)', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);
    const admins = new Set(['root-admin']);

    const closedEvent = {
      id: 'e1',
      orgId: 'org-1',
      topicId: 't1',
      kind: 'close',
      operatorRootId: 'root-admin',
      createdAt: 1
    };
    sdk.docs.query.mockResolvedValue({ items: [{ data: closedEvent }], nextCursor: undefined });
    await expect(
      service.createReply('org-1', 'root-1', { topicId: 't1', content: 'x' }, 'member', admins)
    ).rejects.toThrow(/已关闭/);

    // 同一条 close 事件若出自非管理员，则派生忽略、回复放行
    const forged = { ...closedEvent, operatorRootId: 'root-attacker' };
    sdk.docs.query.mockResolvedValue({ items: [{ data: forged }], nextCursor: undefined });
    await expect(
      service.createReply('org-1', 'root-1', { topicId: 't1', content: 'x' }, 'member', admins)
    ).resolves.toMatchObject({ topicId: 't1' });

    // 隐藏主题同样拦截（档三-14 隐藏=删除诉求表达）
    const hidden = { ...closedEvent, kind: 'hide' };
    sdk.docs.query.mockResolvedValue({ items: [{ data: hidden }], nextCursor: undefined });
    await expect(
      service.createReply('org-1', 'root-1', { topicId: 't1', content: 'x' }, 'member', admins)
    ).rejects.toThrow(/已隐藏/);
  });

  it('rejects new topics on archived or missing boards (edit-as-new-version exempt)', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    // 板块不存在（尚未同步）
    sdk.docs.get.mockResolvedValueOnce(null);
    await expect(
      service.createTopic('org-1', 'root-1', { boardId: 'ghost', title: 't', content: 'c' }, 'member')
    ).rejects.toThrow(/不存在/);

    // 已归档板块拦截新主题
    sdk.docs.get.mockResolvedValueOnce({ id: 'b1', orgId: 'org-1', archived: true });
    await expect(
      service.createTopic('org-1', 'root-1', { boardId: 'b1', title: 't', content: 'c' }, 'member')
    ).rejects.toThrow(/已归档/);

    // 归档板块内编辑自己的历史主题（编辑=新版本）不受限
    const old: ForumTopic = mkTopic({ id: 't1', boardId: 'b1', authorRootId: 'root-1' });
    sdk.docs.get.mockResolvedValueOnce({ id: 'b1', orgId: 'org-1', archived: true });
    const v2 = await service.createTopic(
      'org-1',
      'root-1',
      { boardId: 'b1', title: 't', content: 'c2', supersedes: old },
      'member'
    );
    expect(v2.supersedesId).toBe('t1');
  });

  // ------------------------------------------------------------------
  // 治理事件（档三-11 管理员；档三-14 关闭+隐藏表达删除诉求）
  // ------------------------------------------------------------------

  it('records moderation events (append-only, signed) and rejects non-admins', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const event = await service.createTopicEvent(
      'org-1',
      'root-admin',
      { topicId: 't1', kind: 'close', reason: '偏离主题' },
      'admin'
    );
    expect(event.kind).toBe('close');
    expect(event.reason).toBe('偏离主题');
    expect(event.operatorRootId).toBe('root-admin');
    expect(sdk.docs.put.mock.calls[0][0]).toBe(FORUM_COLLECTIONS.topicEvents);
    // 治理动作签名留痕
    expect(event.signature).toBeDefined();

    // 隐藏（删除诉求表达）同样走事件
    const hidden = await service.createTopicEvent('org-1', 'root-admin', { topicId: 't1', kind: 'hide' }, 'admin');
    expect(hidden.kind).toBe('hide');

    await expect(
      service.createTopicEvent('org-1', 'root-member', { topicId: 't1', kind: 'pin' }, 'member')
    ).rejects.toThrow(/管理员/);
  });

  it('verifies topic event signature with kind+reason payload', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const event = await service.createTopicEvent('org-1', 'root-admin', { topicId: 't1', kind: 'pin', reason: '必读' }, 'admin');
    await expect(service.verifyTopicEventSignature(event)).resolves.toBe(true);
    const expected = buildForumSignPayload('org-1', event.id, 'root-admin', 'pin:必读');
    expect(sdk.identity.verify).toHaveBeenCalledWith(expected, 'sig-1', 'pk-1');

    // 理由被篡改 → 失配
    sdk.identity.verify.mockClear();
    await expect(service.verifyTopicEventSignature({ ...event, reason: '篡改' })).resolves.toBe(false);
    expect(sdk.identity.verify).not.toHaveBeenCalled();
  });

  // ------------------------------------------------------------------
  // message:app（档三-12：仅新主题通知；回复不通知）
  // ------------------------------------------------------------------

  it('sends app message with mandatory summary and topic-card reference after posting', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);

    const topic = await service.createTopic('org-1', 'root-1', { boardId: 'b1', title: '新主题标题', content: '正文' }, 'member');
    await expect(service.notifyNewTopic(topic, '内核')).resolves.toBe(true);

    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(1);
    const [payload, card] = sdk.messages.sendAppMessage.mock.calls[0];
    // 声明式摘要（强制）：未装插件时壳层原生渲染这段文本，前缀带板块名
    expect(payload.summary).toBe('【新主题·内核】新主题标题');
    // 卡片只携带引用 + 定位所需 orgId：正文经 docs 查询，不随消息冗余落库
    expect(card).toEqual({ viewId: 'topic-card', data: { topicId: topic.id, orgId: 'org-1' } });
  });

  it('degrades to false when app message is denied or rate-limited (posting not blocked)', async () => {
    const sdk = createMockSdk();
    sdk.messages.sendAppMessage.mockRejectedValueOnce(new Error('rate-limited'));
    const service = new ForumService(sdk);

    const topic = mkTopic({ id: 'topic-rl' });
    await expect(service.notifyNewTopic(topic, '内核')).resolves.toBe(false);
  });

  it('returns false when messages module is absent (non-bridge context)', async () => {
    const sdk = createMockSdk();
    delete sdk.messages;
    const service = new ForumService(sdk);

    await expect(service.notifyNewTopic(mkTopic(), '内核')).resolves.toBe(false);
  });

  it('generates local notifications only for new first-version topics and dedups across loads (档三-12)', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);
    const boards = new Map([['board-1', '内核']]);

    const t1 = mkTopic({ id: 't1', orgId: 'org-notify' });
    const t2 = mkTopic({ id: 't2', orgId: 'org-notify' });
    // 编辑产生的新版本不通知
    const t2v2 = mkTopic({ id: 't2v2', orgId: 'org-notify', supersedesId: 't2' });

    await expect(service.notifyNewTopics('org-notify', [t1, t2, t2v2], boards)).resolves.toBe(2);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(2);
    expect(sdk.messages.sendAppMessage.mock.calls[0][1].data).toEqual({ topicId: 't1', orgId: 'org-notify' });

    // 再次加载：已通知过的不再生成，仅新主题 t3 补一条
    const t3 = mkTopic({ id: 't3', orgId: 'org-notify' });
    await expect(service.notifyNewTopics('org-notify', [t1, t2, t3], boards)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(3);
  });

  it('stops local generation batch on send failure and retries unmarked topics later', async () => {
    const sdk = createMockSdk();
    const service = new ForumService(sdk);
    const boards = new Map<string, string>();

    const t1 = mkTopic({ id: 't1', orgId: 'org-rl' });
    const t2 = mkTopic({ id: 't2', orgId: 'org-rl' });

    // 第二条触发限流：本轮中止，t1 已记账、t2 未记账
    sdk.messages.sendAppMessage.mockResolvedValueOnce({ id: 'm1' }).mockRejectedValueOnce(new Error('rate-limited'));
    await expect(service.notifyNewTopics('org-rl', [t1, t2], boards)).resolves.toBe(1);

    // 下次加载时只补发未记账的 t2
    await expect(service.notifyNewTopics('org-rl', [t1, t2], boards)).resolves.toBe(1);
    expect(sdk.messages.sendAppMessage).toHaveBeenCalledTimes(3);
    expect(sdk.messages.sendAppMessage.mock.calls[2][0].topicId).toBe('t2');
  });

  // ------------------------------------------------------------------
  // 查询（三层结构：orgId 一级过滤，boardId/topicId 二级过滤由视图层组合）
  // ------------------------------------------------------------------

  it('queries all collections by orgId to keep cross-device sync scope stable', async () => {
    const sdk = createMockSdk();
    sdk.docs.query.mockResolvedValue({ items: [], nextCursor: undefined });

    const service = new ForumService(sdk);
    await service.loadBoards('org-xyz');
    await service.loadTopics('org-xyz');
    await service.loadReplies('org-xyz');
    await service.loadTopicEvents('org-xyz');

    const expectedCollections = [
      FORUM_COLLECTIONS.boards,
      FORUM_COLLECTIONS.topics,
      FORUM_COLLECTIONS.replies,
      FORUM_COLLECTIONS.topicEvents
    ];
    sdk.docs.query.mock.calls.forEach((call: any[], index: number) => {
      expect(call[0]).toBe(expectedCollections[index]);
      expect(call[1].filter[0]).toEqual({ field: 'orgId', value: 'org-xyz' });
    });
  });

  it('sorts boards by sort weight then creation time', async () => {
    const sdk = createMockSdk();
    const boards: ForumBoard[] = [
      { id: 'b2', orgId: 'org-1', name: 'B', intro: '', sort: 10, archived: false, createdBy: 'r', createdAt: 1, updatedAt: 1 },
      { id: 'b1', orgId: 'org-1', name: 'A', intro: '', sort: 0, archived: false, createdBy: 'r', createdAt: 2, updatedAt: 2 }
    ];
    sdk.docs.query.mockResolvedValue({ items: boards.map((data) => ({ data })), nextCursor: undefined });

    const service = new ForumService(sdk);
    const loaded = await service.loadBoards('org-1');
    expect(loaded.map((board) => board.id)).toEqual(['b1', 'b2']);
  });
});
