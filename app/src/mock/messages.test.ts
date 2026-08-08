// mock/messages onChatReceived 单测：ChatReceived 事件的会话快照语义
// （conversation 是内核回写本条消息后的权威快照，前端始终信任、不再本地 +1）
import { describe, it, expect } from 'vitest';
import {
  onChatReceived,
  getConversation,
  getMessages,
  getAppMessages,
  markRead,
  openConversation,
  closeConversation,
  sendAppMessageLocal,
  totalUnread,
  hasUnreadMessages,
  mergeAppMessages,
  unreadCountOf,
  type ChatMessage,
  type Conversation
} from '../stores/messages';
import { toggleAppConversationBlocked } from '../stores/app-conversations';
import type { AppMessageDto } from '../api/types';

let fixtureSeq = 0;

function makeConversation(overrides: Partial<Conversation> = {}): Conversation {
  fixtureSeq += 1;
  return {
    id: `dm:peer-${fixtureSeq}`,
    kind: 'direct',
    title: `对方${fixtureSeq}`,
    peerId: `peer-${fixtureSeq}`,
    unreadCount: 0,
    pinnedAt: 0,
    muted: false,
    online: false,
    draft: '',
    updatedAt: 1_000,
    ...overrides
  };
}

function makeMessage(conversation: Conversation, overrides: Partial<ChatMessage> = {}): ChatMessage {
  fixtureSeq += 1;
  return {
    id: `m-${fixtureSeq}`,
    senderId: conversation.peerId,
    senderName: conversation.title,
    type: 'text',
    content: '你好',
    createdAt: conversation.updatedAt,
    recalled: false,
    ...overrides
  };
}

describe('onChatReceived（信任内核会话快照）', () => {
  it('新建会话：快照 unreadCount 已含本条消息，不再本地 +1（修复未读翻倍）', () => {
    const key = 'test:chat-new';
    // 内核快照：本条消息已回写进去 → unreadCount=1
    const conversation = makeConversation({ unreadCount: 1, updatedAt: 2_000 });
    onChatReceived({ spaceKey: key, conversation, message: makeMessage(conversation) });
    const conv = getConversation(key, conversation.id);
    expect(conv?.unreadCount).toBe(1);
    expect(conv?.updatedAt).toBe(2_000);
  });

  it('已有会话：unreadCount 直接取快照回写值，而非本地累加', () => {
    const key = 'test:chat-existing';
    const first = makeConversation({ unreadCount: 1, updatedAt: 2_000 });
    onChatReceived({ spaceKey: key, conversation: first, message: makeMessage(first) });
    // 第二条消息：内核快照回写后 unreadCount=2
    const snapshot = { ...first, unreadCount: 2, updatedAt: 3_000 };
    onChatReceived({ spaceKey: key, conversation: snapshot, message: makeMessage(snapshot) });
    const conv = getConversation(key, first.id);
    expect(conv?.unreadCount).toBe(2);
    expect(conv?.updatedAt).toBe(3_000);
    expect(getMessages(key, first.id)).toHaveLength(2);
  });

  it('消息按 id 去重入列，未读仍以快照为准', () => {
    const key = 'test:chat-dup';
    const conversation = makeConversation({ unreadCount: 1, updatedAt: 2_000 });
    const message = makeMessage(conversation);
    onChatReceived({ spaceKey: key, conversation, message });
    onChatReceived({ spaceKey: key, conversation, message });
    expect(getMessages(key, conversation.id)).toHaveLength(1);
    expect(getConversation(key, conversation.id)?.unreadCount).toBe(1);
  });

  it('活跃会话：清零未读，updatedAt 仍取快照', () => {
    const key = 'test:chat-active';
    const conversation = makeConversation({ unreadCount: 0, updatedAt: 1_000 });
    onChatReceived({ spaceKey: key, conversation, message: makeMessage(conversation) });
    openConversation(key, conversation.id);
    const snapshot = { ...conversation, unreadCount: 1, updatedAt: 2_000 };
    onChatReceived({ spaceKey: key, conversation: snapshot, message: makeMessage(snapshot) });
    const conv = getConversation(key, conversation.id);
    expect(conv?.unreadCount).toBe(0);
    expect(conv?.updatedAt).toBe(2_000);
    closeConversation(key);
  });

  it("自设备同步来的消息（senderId='me'）：内核不计未读，快照为 0 即保持 0", () => {
    const key = 'test:chat-self';
    const conversation = makeConversation({ unreadCount: 0, updatedAt: 2_000 });
    const message = makeMessage(conversation, { senderId: 'me', senderName: '我', status: 'sent' });
    onChatReceived({ spaceKey: key, conversation, message });
    const conv = getConversation(key, conversation.id);
    expect(conv?.unreadCount).toBe(0);
    const stored = getMessages(key, conversation.id)[0];
    expect(stored.senderId).toBe('me');
  });

  it('已有会话：merge 快照里的 online（上下线随后续消息快照刷新）', () => {
    const key = 'test:chat-online';
    const first = makeConversation({ online: false, updatedAt: 2_000 });
    onChatReceived({ spaceKey: key, conversation: first, message: makeMessage(first) });
    expect(getConversation(key, first.id)?.online).toBe(false);
    const snapshot = { ...first, online: true, updatedAt: 3_000 };
    onChatReceived({ spaceKey: key, conversation: snapshot, message: makeMessage(snapshot) });
    const conv = getConversation(key, first.id);
    expect(conv?.online).toBe(true);
    expect(conv?.updatedAt).toBe(3_000);
  });
});

describe('应用消息内存镜像（§20，非 Tauri 环境）', () => {
  it('summary 校验先于落库：缺失/空白/超长一律拒绝（错误前缀与内核一致）', () => {
    const key = 'test:app-validate';
    expect(() => sendAppMessageLocal(key, 'spark-example', {})).toThrow(/^missing-summary/);
    expect(() => sendAppMessageLocal(key, 'spark-example', { summary: '   ' })).toThrow(/^missing-summary/);
    expect(() => sendAppMessageLocal(key, 'spark-example', { summary: 'x'.repeat(201) })).toThrow(/^summary-too-long/);
    expect(getConversation(key, 'app:spark-example')).toBeUndefined();
  });

  it('写入即建会话（惰性创建），summary 为 trim 后文本，未读 +1', () => {
    const key = 'test:app-send';
    const dto = sendAppMessageLocal(key, 'spark-example', { summary: '  新微博  ', text: 'hello' });
    expect(dto.summary).toBe('新微博');
    expect(dto.status).toBe('local');
    expect(dto.read).toBe(false);
    const conv = getConversation(key, 'app:spark-example');
    expect(conv?.kind).toBe('app');
    expect(conv?.peerId).toBe('spark-example');
    expect(conv?.unreadCount).toBe(1);
    expect(getAppMessages(key, 'app:spark-example')).toHaveLength(1);
  });

  it('限流：同空间同插件 60s 窗口内第 11 条拒绝（rate-limited）', () => {
    const key = 'test:app-rl';
    for (let i = 0; i < 10; i += 1) {
      sendAppMessageLocal(key, 'spark-example', { summary: `第${i + 1}条` });
    }
    expect(() => sendAppMessageLocal(key, 'spark-example', { summary: '超限' })).toThrow(/^rate-limited/);
    expect(getAppMessages(key, 'app:spark-example')).toHaveLength(10);
    // 另一插件/另一空间不受影响
    sendAppMessageLocal(key, 'other-plugin', { summary: 'ok' });
    sendAppMessageLocal('test:app-rl-2', 'spark-example', { summary: 'ok' });
  });

  it('markRead：应用会话清零未读并把会话内消息批量置 read', () => {
    const key = 'test:app-read';
    sendAppMessageLocal(key, 'spark-example', { summary: '一' });
    sendAppMessageLocal(key, 'spark-example', { summary: '二' });
    expect(getConversation(key, 'app:spark-example')?.unreadCount).toBe(2);
    markRead(key, 'app:spark-example');
    expect(getConversation(key, 'app:spark-example')?.unreadCount).toBe(0);
    expect(getAppMessages(key, 'app:spark-example').every((msg) => msg.read)).toBe(true);
  });

  it('活跃会话写入：未读保持清零（与 onChatReceived 同口径）', () => {
    const key = 'test:app-active';
    sendAppMessageLocal(key, 'spark-example', { summary: '首条' });
    openConversation(key, 'app:spark-example');
    sendAppMessageLocal(key, 'spark-example', { summary: '次条' });
    expect(getConversation(key, 'app:spark-example')?.unreadCount).toBe(0);
    expect(getAppMessages(key, 'app:spark-example')[1].read).toBe(true);
    closeConversation(key);
  });

  it('被屏蔽的应用会话不参与未读聚合（取消屏蔽后恢复）', () => {
    const key = 'test:app-blocked';
    sendAppMessageLocal(key, 'spark-example', { summary: '通知' });
    expect(unreadCountOf(key)).toBe(1);
    toggleAppConversationBlocked(key, 'spark-example');
    expect(unreadCountOf(key)).toBe(0);
    toggleAppConversationBlocked(key, 'spark-example');
    expect(unreadCountOf(key)).toBe(1);
  });
});

describe('应用消息内存镜像补充（pluginId 校验 / system 豁免 / 屏蔽聚合 / merge 排序）', () => {
  it('pluginId 校验：非法 id 一律拒绝（invalid-plugin-id，与内核错误前缀一致）', () => {
    const key = 'test:app-pid';
    expect(() => sendAppMessageLocal(key, 'Spark-Example', { summary: 'x' })).toThrow(/^invalid-plugin-id/);
    expect(() => sendAppMessageLocal(key, 'evil/plugin', { summary: 'x' })).toThrow(/^invalid-plugin-id/);
    expect(() => sendAppMessageLocal(key, '-bad', { summary: 'x' })).toThrow(/^invalid-plugin-id/);
    // 校验先于落库与限流：不产生会话
    expect(getConversation(key, 'app:Spark-Example')).toBeUndefined();
  });

  it('内置 system 会话豁免限流（与内核 message_app_send 同口径）', () => {
    const key = 'test:app-rl-system';
    for (let i = 0; i < 12; i += 1) {
      sendAppMessageLocal(key, 'system', { summary: `系统通知${i}` });
    }
    expect(getAppMessages(key, 'app:system')).toHaveLength(12);
  });

  it('totalUnread/hasUnreadMessages：被屏蔽会话不计入聚合（取消屏蔽恢复）', () => {
    const key = 'test:app-blocked-agg';
    const baseline = totalUnread.value;
    sendAppMessageLocal(key, 'spark-example', { summary: '通知' });
    expect(hasUnreadMessages(key)).toBe(true);
    expect(totalUnread.value).toBe(baseline + 1);
    toggleAppConversationBlocked(key, 'spark-example');
    expect(hasUnreadMessages(key)).toBe(false);
    expect(totalUnread.value).toBe(baseline);
    toggleAppConversationBlocked(key, 'spark-example');
    expect(totalUnread.value).toBe(baseline + 1);
  });

  it('mergeAppMessages：按 id 合并后按 createdAt 升序归位（水合期间本地新增不串序）', () => {
    const key = 'test:app-merge';
    const local = sendAppMessageLocal(key, 'spark-example', { summary: '本地新消息' });
    const history = (id: string, createdAt: number): AppMessageDto => ({
      id,
      pluginId: 'spark-example',
      summary: id,
      payload: { summary: id },
      createdAt,
      status: 'local',
      read: true
    });
    // 内核快照只含水合前的历史（时间早于本地新增）
    mergeAppMessages(key, 'app:spark-example', [
      history('h1', local.createdAt - 2000),
      history('h2', local.createdAt - 1000)
    ]);
    expect(getAppMessages(key, 'app:spark-example').map((m) => m.id)).toEqual(['h1', 'h2', local.id]);
  });
});
