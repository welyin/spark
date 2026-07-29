// mock/messages onChatReceived 单测：ChatReceived 事件的会话快照语义
// （conversation 是内核回写本条消息后的权威快照，前端始终信任、不再本地 +1）
import { describe, it, expect } from 'vitest';
import {
  onChatReceived,
  getConversation,
  getMessages,
  openConversation,
  closeConversation,
  type ChatMessage,
  type Conversation
} from './messages';

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
});
