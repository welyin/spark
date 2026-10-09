import { describe, expect, it } from 'vitest';
import {
  FORUM_MAX_REPLY_CONTENT_LENGTH,
  FORUM_MAX_TOPIC_CONTENT_LENGTH,
  buildForumSignPayload,
  buildReplyThread,
  buildTopicList,
  buildTopicSummary,
  canEditTopic,
  canManageBoards,
  canModerateTopics,
  canPostTopic,
  deriveTopicState,
  hashForumContent,
  resolveLatestTopics,
  splitAffairRefs,
  topicVersionHistory,
  validateBoardInput,
  validateReplyContent,
  validateTopicContent,
  validateTopicTitle,
  type ForumReply,
  type ForumTopic,
  type ForumTopicEvent
} from '../model';

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

const mkEvent = (overrides: Partial<ForumTopicEvent> = {}): ForumTopicEvent => ({
  id: `event-${Math.random()}`,
  orgId: 'org-1',
  topicId: 'topic-1',
  kind: 'pin',
  operatorRootId: 'root-admin',
  createdAt: 1,
  ...overrides
});

const mkReply = (overrides: Partial<ForumReply> = {}): ForumReply => ({
  id: `reply-${Math.random()}`,
  orgId: 'org-1',
  topicId: 'topic-1',
  content: '回复',
  authorRootId: 'root-1',
  createdAt: 1,
  ...overrides
});

describe('spark-forum model', () => {
  // ------------------------------------------------------------------
  // 权限（§5 需改造：发帖权限反转为全员，管理员权限移到板块/主题治理）
  // ------------------------------------------------------------------

  it('allows all org members to post topics and replies (permission inversion from spark-example)', () => {
    expect(canPostTopic('admin')).toBe(true);
    expect(canPostTopic('member')).toBe(true);
    expect(canPostTopic(null)).toBe(false);
    expect(canPostTopic(undefined)).toBe(false);
  });

  it('restricts board management and topic moderation to org admins (档三-11 名册管理员)', () => {
    expect(canManageBoards('admin')).toBe(true);
    expect(canManageBoards('member')).toBe(false);
    expect(canModerateTopics('admin')).toBe(true);
    expect(canModerateTopics('member')).toBe(false);
    expect(canModerateTopics(null)).toBe(false);
  });

  it('allows only the author to edit a topic (edit-as-new-version)', () => {
    const topic = mkTopic({ authorRootId: 'root-author' });
    expect(canEditTopic(topic, 'root-author')).toBe(true);
    expect(canEditTopic(topic, 'root-other')).toBe(false);
    expect(canEditTopic(topic, null)).toBe(false);
  });

  // ------------------------------------------------------------------
  // 长度约束（参数化集中在 model 层）
  // ------------------------------------------------------------------

  it('enforces topic title/content length constraints (20000-char body)', () => {
    expect(validateTopicTitle('  ').ok).toBe(false);
    expect(validateTopicTitle('a'.repeat(120)).ok).toBe(true);
    expect(validateTopicTitle('a'.repeat(121)).ok).toBe(false);
    expect(validateTopicContent('a'.repeat(FORUM_MAX_TOPIC_CONTENT_LENGTH)).ok).toBe(true);
    expect(validateTopicContent('a'.repeat(FORUM_MAX_TOPIC_CONTENT_LENGTH + 1)).ok).toBe(false);
    expect(validateTopicContent('   ').ok).toBe(false);
  });

  it('enforces reply length constraint (5000 chars)', () => {
    expect(validateReplyContent('a'.repeat(FORUM_MAX_REPLY_CONTENT_LENGTH)).ok).toBe(true);
    expect(validateReplyContent('a'.repeat(FORUM_MAX_REPLY_CONTENT_LENGTH + 1)).ok).toBe(false);
    expect(validateReplyContent('').ok).toBe(false);
  });

  it('validates board name/intro', () => {
    expect(validateBoardInput('内核', '内核讨论')).toEqual({ ok: true });
    expect(validateBoardInput('  ', 'x').ok).toBe(false);
    expect(validateBoardInput('a'.repeat(41), 'x').ok).toBe(false);
    expect(validateBoardInput('ok', 'a'.repeat(201)).ok).toBe(false);
  });

  // ------------------------------------------------------------------
  // 签名载荷（直接复用 spark-example 四元绑定模式）
  // ------------------------------------------------------------------

  it('hashes content deterministically (stable sign payload material)', () => {
    expect(hashForumContent('hello spark')).toBe(hashForumContent('hello spark'));
    expect(hashForumContent('hello spark')).not.toBe(hashForumContent('hello sparx'));
    expect(hashForumContent('')).toMatch(/^[0-9a-f]{8}$/);
  });

  it('binds sign payload to org + record + author + content hash (anti replay, anti author-swap)', () => {
    const payload = buildForumSignPayload('org-1', 'topic-1', 'root-admin', '正文');
    expect(payload).toBe(`org-1:topic-1:root-admin:${hashForumContent('正文')}`);
    expect(buildForumSignPayload('org-1', 'topic-2', 'root-admin', '正文')).not.toBe(payload);
    expect(buildForumSignPayload('org-2', 'topic-1', 'root-admin', '正文')).not.toBe(payload);
    expect(buildForumSignPayload('org-1', 'topic-1', 'root-other', '正文')).not.toBe(payload);
  });

  // ------------------------------------------------------------------
  // 消息摘要（§5 需改造：带板块名前缀；summary ≤200 字符约束不变）
  // ------------------------------------------------------------------

  it('builds self-contained new-topic summary with board name (declarative fallback text)', () => {
    expect(buildTopicSummary('内核', 'SDK 桥协议要不要加事件订阅')).toBe('【新主题·内核】SDK 桥协议要不要加事件订阅');
    const long = '长'.repeat(300);
    const summary = buildTopicSummary('内核', long);
    expect(summary.length).toBeLessThanOrEqual(200);
    expect(summary.endsWith('…')).toBe(true);
    expect(summary.startsWith('【新主题·内核】')).toBe(true);
    // 板块名为空时降级
    expect(buildTopicSummary('  ', '标题')).toBe('【新主题·未分板块】标题');
  });

  // ------------------------------------------------------------------
  // 两级楼中楼（直接复用 buildCommentThread 逻辑，字段改名）
  // ------------------------------------------------------------------

  it('builds two-level reply thread in chronological order', () => {
    const replies: ForumReply[] = [
      mkReply({ id: 'r2', topicId: 't1', parentReplyId: 'r1', content: 'child', createdAt: 3 }),
      mkReply({ id: 'r1', topicId: 't1', content: 'root', createdAt: 1 }),
      mkReply({ id: 'r3', topicId: 't1', content: 'root-2', createdAt: 2 }),
      mkReply({ id: 'r4', topicId: 't2', content: 'other topic', createdAt: 1 })
    ];

    const thread = buildReplyThread('t1', replies);
    expect(thread).toHaveLength(2);
    expect(thread[0].reply.id).toBe('r1');
    expect(thread[0].replies).toHaveLength(1);
    expect(thread[0].replies[0].id).toBe('r2');
    expect(thread[1].reply.id).toBe('r3');
  });

  it('keeps orphaned replies (dangling parent) as root floors instead of dropping them', () => {
    const replies: ForumReply[] = [
      mkReply({ id: 'r1', topicId: 't1', parentReplyId: 'ghost', content: '孤儿', createdAt: 2 }),
      mkReply({ id: 'r0', topicId: 't1', content: 'root', createdAt: 1 })
    ];
    const thread = buildReplyThread('t1', replies);
    expect(thread.map((node) => node.reply.id).sort()).toEqual(['r0', 'r1']);
  });

  // ------------------------------------------------------------------
  // 事件流状态派生（需新增：spark-example 没有的能力）
  // ------------------------------------------------------------------

  it('derives topic state from event stream in chronological order', () => {
    const admins = new Set(['root-admin']);
    const events: ForumTopicEvent[] = [
      mkEvent({ topicId: 't1', kind: 'pin', createdAt: 1 }),
      mkEvent({ topicId: 't1', kind: 'feature', createdAt: 2 }),
      mkEvent({ topicId: 't1', kind: 'resolve', createdAt: 3 })
    ];
    expect(deriveTopicState('t1', events, admins)).toEqual({
      pinned: true,
      featured: true,
      resolved: true,
      closed: false,
      hidden: false
    });

    // 其他主题的事件不影响本主题
    expect(deriveTopicState('t2', events, admins)).toEqual({
      pinned: false,
      featured: false,
      resolved: false,
      closed: false,
      hidden: false
    });
  });

  it('later events override earlier ones (unpin/close/reopen/hide)', () => {
    const admins = new Set(['root-admin']);
    const events: ForumTopicEvent[] = [
      mkEvent({ kind: 'pin', createdAt: 1 }),
      mkEvent({ kind: 'unpin', createdAt: 2 }),
      mkEvent({ kind: 'resolve', createdAt: 3 }),
      mkEvent({ kind: 'close', createdAt: 4 })
    ];
    const state = deriveTopicState('topic-1', events, admins);
    expect(state.pinned).toBe(false);
    // close 清除 resolved
    expect(state.resolved).toBe(false);
    expect(state.closed).toBe(true);

    const reopened = deriveTopicState('topic-1', [...events, mkEvent({ kind: 'reopen', createdAt: 5 })], admins);
    expect(reopened.closed).toBe(false);

    const hidden = deriveTopicState('topic-1', [...events, mkEvent({ kind: 'hide', createdAt: 5 })], admins);
    expect(hidden.hidden).toBe(true);
    const unhidden = deriveTopicState('topic-1', [
      ...events,
      mkEvent({ kind: 'hide', createdAt: 5 }),
      mkEvent({ kind: 'unhide', createdAt: 6 })
    ], admins);
    expect(unhidden.hidden).toBe(false);
  });

  it('ignores governance events from non-roster-admin operators (read-side authz, fail-closed)', () => {
    // 恶意成员伪造 close 事件不能篡改全员视图；合法管理员的 pin 照常生效
    const events: ForumTopicEvent[] = [
      mkEvent({ id: 'e1', kind: 'close', operatorRootId: 'root-attacker', createdAt: 1 }),
      mkEvent({ id: 'e2', kind: 'pin', operatorRootId: 'root-admin', createdAt: 2 }),
      mkEvent({ id: 'e3', kind: 'hide', operatorRootId: 'root-member', createdAt: 3 })
    ];
    const state = deriveTopicState('topic-1', events, new Set(['root-admin']));
    expect(state.closed).toBe(false);
    expect(state.hidden).toBe(false);
    expect(state.pinned).toBe(true);

    // 管理员集合为空 = 全部治理事件失效（fail-closed）
    expect(deriveTopicState('topic-1', events, new Set())).toEqual({
      pinned: false,
      featured: false,
      resolved: false,
      closed: false,
      hidden: false
    });
  });

  it('orders events deterministically across devices (createdAt tie broken by event.id)', () => {
    const admins = new Set(['root-admin']);
    // 同 createdAt 平局：字典序大的 id 后生效（unpin 覆盖 pin），各设备一致
    const events: ForumTopicEvent[] = [
      mkEvent({ id: 'ev-b', kind: 'unpin', createdAt: 5 }),
      mkEvent({ id: 'ev-a', kind: 'pin', createdAt: 5 })
    ];
    expect(deriveTopicState('topic-1', events, admins).pinned).toBe(false);
    // 输入顺序不同，派生结果不变
    expect(deriveTopicState('topic-1', [...events].reverse(), admins).pinned).toBe(false);
  });

  // ------------------------------------------------------------------
  // 编辑=新版本（需新增：supersedesId 版本链）
  // ------------------------------------------------------------------

  it('resolves only the latest version per edit lineage', () => {
    const v1 = mkTopic({ id: 't1', content: '初版', createdAt: 1 });
    const v2 = mkTopic({ id: 't2', content: '修订版', supersedesId: 't1', createdAt: 2 });
    const v3 = mkTopic({ id: 't3', content: '终版', supersedesId: 't2', createdAt: 3 });
    const other = mkTopic({ id: 't9', content: '别的主题', createdAt: 1 });

    const latest = resolveLatestTopics([v1, v2, v3, other]);
    expect(latest.map((topic) => topic.id).sort()).toEqual(['t3', 't9']);
  });

  it('keeps broken chains (supersedesId pointing nowhere) as standalone lineage', () => {
    const orphan = mkTopic({ id: 't5', supersedesId: 'ghost', createdAt: 1 });
    expect(resolveLatestTopics([orphan]).map((topic) => topic.id)).toEqual(['t5']);
  });

  it('breaks lineage on author change (anti hijack: supersedesId to others topic)', () => {
    // 恶意成员发 supersedesId 指向他人主题最新版的记录抢链：
    // 链在作者变更处断开，t2 独立成谱系，原作者的 t1 头部不受影响
    const original = mkTopic({ id: 't1', authorRootId: 'root-author', content: '正主', createdAt: 1 });
    const hijack = mkTopic({
      id: 't2',
      authorRootId: 'root-attacker',
      content: '篡改',
      supersedesId: 't1',
      createdAt: 2
    });

    const latest = resolveLatestTopics([original, hijack]);
    expect(latest.map((topic) => topic.id).sort()).toEqual(['t1', 't2']);

    // 正反两面的中间链：作者自身修订照常归并，混入的他人记录断开
    const v2 = mkTopic({ id: 't1v2', authorRootId: 'root-author', supersedesId: 't1', createdAt: 2 });
    const hijack2 = mkTopic({ id: 't3', authorRootId: 'root-attacker', supersedesId: 't1v2', createdAt: 3 });
    const latest2 = resolveLatestTopics([original, v2, hijack2]);
    expect(latest2.map((topic) => topic.id).sort()).toEqual(['t1v2', 't3']);
  });

  it('version history stops at author change (hijacked versions excluded)', () => {
    const v1 = mkTopic({ id: 't1', authorRootId: 'root-author', createdAt: 1 });
    const v2 = mkTopic({ id: 't2', authorRootId: 'root-author', supersedesId: 't1', createdAt: 2 });
    const hijack = mkTopic({ id: 't3', authorRootId: 'root-attacker', supersedesId: 't2', createdAt: 3 });
    const all = [v1, v2, hijack];

    // 前推遇抢链记录即停：历史只含同作者版本
    expect(topicVersionHistory(v2, all).map((topic) => topic.id)).toEqual(['t1', 't2']);
    // 抢链记录自身的历史也回溯不到他人版本
    expect(topicVersionHistory(hijack, all).map((topic) => topic.id)).toEqual(['t3']);
  });

  it('builds full version history from any version in the chain', () => {
    const v1 = mkTopic({ id: 't1', createdAt: 1 });
    const v2 = mkTopic({ id: 't2', supersedesId: 't1', createdAt: 2 });
    const v3 = mkTopic({ id: 't3', supersedesId: 't2', createdAt: 3 });
    const all = [v1, v2, v3];

    expect(topicVersionHistory(v3, all).map((topic) => topic.id)).toEqual(['t1', 't2', 't3']);
    // 从中间版本出发也能拿到完整历史（向前回溯 + 向后前推）
    expect(topicVersionHistory(v2, all).map((topic) => topic.id)).toEqual(['t1', 't2', 't3']);
  });

  // ------------------------------------------------------------------
  // 主题列表排序（置顶优先 + 最近活跃倒序；隐藏沉底）
  // ------------------------------------------------------------------

  it('sorts board topic list: pinned first, then last-active desc; hidden sinks to bottom', () => {
    const topics: ForumTopic[] = [
      mkTopic({ id: 't1', boardId: 'b1', createdAt: 1 }),
      mkTopic({ id: 't2', boardId: 'b1', createdAt: 2 }),
      mkTopic({ id: 't3', boardId: 'b1', createdAt: 3 }),
      mkTopic({ id: 't4', boardId: 'b2', createdAt: 4 })
    ];
    const events: ForumTopicEvent[] = [
      mkEvent({ topicId: 't1', kind: 'pin', createdAt: 10 }),
      mkEvent({ topicId: 't2', kind: 'hide', createdAt: 10 })
    ];
    const replies: ForumReply[] = [mkReply({ topicId: 't1', createdAt: 5 })];

    const list = buildTopicList('b1', topics, events, replies, new Set(['root-admin']));
    expect(list.map((item) => item.topic.id)).toEqual(['t1', 't3', 't2']);
    expect(list[0].state.pinned).toBe(true);
    expect(list[0].replyCount).toBe(1);
    expect(list[0].lastActiveAt).toBe(5);
    expect(list[2].state.hidden).toBe(true);
  });

  // ------------------------------------------------------------------
  // 议题引用（需新增，MVP 展示级纯文本引用）
  // ------------------------------------------------------------------

  it('splits affair references out of plain content for display-level rendering', () => {
    const segments = splitAffairRefs('方案已成熟，见 affair:ab12cd34 与 affair:xyz_5678 两个议题。');
    expect(segments).toEqual([
      { type: 'text', text: '方案已成熟，见 ' },
      { type: 'affair-ref', text: 'affair:ab12cd34' },
      { type: 'text', text: ' 与 ' },
      { type: 'affair-ref', text: 'affair:xyz_5678' },
      { type: 'text', text: ' 两个议题。' }
    ]);
    // 无引用时整段为文本
    expect(splitAffairRefs('普通正文')).toEqual([{ type: 'text', text: '普通正文' }]);
    // 过短的 id 不匹配（防误伤普通文本里的 "affair:" 字样）
    expect(splitAffairRefs('affair:ab 太短')).toEqual([{ type: 'text', text: 'affair:ab 太短' }]);
  });
});
