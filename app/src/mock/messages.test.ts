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
  unreadCountOf,
  type ChatMessage,
  type Conversation
} from './messages';
import { toggleAppConversationBlocked } from '../stores/app-conversations';

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
    expect(() => sendAppMessageLocal(key, 'weibo-core', {})).toThrow(/^missing-summary/);
    expect(() => sendAppMessageLocal(key, 'weibo-core', { summary: '   ' })).toThrow(/^missing-summary/);
    expect(() => sendAppMessageLocal(key, 'weibo-core', { summary: 'x'.repeat(201) })).toThrow(/^summary-too-long/);
    expect(getConversation(key, 'app:weibo-core')).toBeUndefined();
  });

  it('写入即建会话（惰性创建），summary 为 trim 后文本，未读 +1', () => {
    const key = 'test:app-send';
    const dto = sendAppMessageLocal(key, 'weibo-core', { summary: '  新微博  ', text: 'hello' });
    expect(dto.summary).toBe('新微博');
    expect(dto.status).toBe('local');
    expect(dto.read).toBe(false);
    const conv = getConversation(key, 'app:weibo-core');
    expect(conv?.kind).toBe('app');
    expect(conv?.peerId).toBe('weibo-core');
    expect(conv?.unreadCount).toBe(1);
    expect(getAppMessages(key, 'app:weibo-core')).toHaveLength(1);
  });

  it('限流：同空间同插件 60s 窗口内第 11 条拒绝（rate-limited）', () => {
    const key = 'test:app-rl';
    for (let i = 0; i < 10; i += 1) {
      sendAppMessageLocal(key, 'weibo-core', { summary: `第${i + 1}条` });
    }
    expect(() => sendAppMessageLocal(key, 'weibo-core', { summary: '超限' })).toThrow(/^rate-limited/);
    expect(getAppMessages(key, 'app:weibo-core')).toHaveLength(10);
    // 另一插件/另一空间不受影响
    sendAppMessageLocal(key, 'other-plugin', { summary: 'ok' });
    sendAppMessageLocal('test:app-rl-2', 'weibo-core', { summary: 'ok' });
  });

  it('markRead：应用会话清零未读并把会话内消息批量置 read', () => {
    const key = 'test:app-read';
    sendAppMessageLocal(key, 'weibo-core', { summary: '一' });
    sendAppMessageLocal(key, 'weibo-core', { summary: '二' });
    expect(getConversation(key, 'app:weibo-core')?.unreadCount).toBe(2);
    markRead(key, 'app:weibo-core');
    expect(getConversation(key, 'app:weibo-core')?.unreadCount).toBe(0);
    expect(getAppMessages(key, 'app:weibo-core').every((msg) => msg.read)).toBe(true);
  });

  it('活跃会话写入：未读保持清零（与 onChatReceived 同口径）', () => {
    const key = 'test:app-active';
    sendAppMessageLocal(key, 'weibo-core', { summary: '首条' });
    openConversation(key, 'app:weibo-core');
    sendAppMessageLocal(key, 'weibo-core', { summary: '次条' });
    expect(getConversation(key, 'app:weibo-core')?.unreadCount).toBe(0);
    expect(getAppMessages(key, 'app:weibo-core')[1].read).toBe(true);
    closeConversation(key);
  });

  it('被屏蔽的应用会话不参与未读聚合（取消屏蔽后恢复）', () => {
    const key = 'test:app-blocked';
    sendAppMessageLocal(key, 'weibo-core', { summary: '通知' });
    expect(unreadCountOf(key)).toBe(1);
    toggleAppConversationBlocked(key, 'weibo-core');
    expect(unreadCountOf(key)).toBe(0);
    toggleAppConversationBlocked(key, 'weibo-core');
    expect(unreadCountOf(key)).toBe(1);
  });
});
