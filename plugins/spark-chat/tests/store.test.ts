/**
 * spark-chat 消息 store 单测（功能对等迁移自壳层 stores/messages）：
 * - 事件快照语义（onChatReceived 信任内核会话快照）与壳层同口径回归；
 * - 数据面经 sdk.messages 通路：首次访问水合、发送乐观入列 + 内核回写、
 *   同 id 合并不重复（换前端数据原样在的最小验证：store 只经 sdk.messages
 *   读写，fake SDK 后端的同一数据可被另一读取方原样读到）。
 */
import { describe, expect, it } from 'vitest';
import type { PluginContext, PluginMessagesAPI, PluginSDK } from '../../../packages/plugin-sdk/src';
import { bindPluginRuntime } from '../src/sdk-host';
import {
  closeConversation,
  getConversation,
  getMessages,
  listConversations,
  markRead,
  onChatReceived,
  openConversation,
  resetMessagesCache,
  sendText,
  unreadCountOf,
  type ChatMessage,
  type Conversation
} from '../src/store';

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

describe('onChatReceived（信任内核会话快照，与壳层同口径）', () => {
  it('新建会话：快照 unreadCount 已含本条消息，不再本地 +1', () => {
    const key = 'test:chat-new';
    const conversation = makeConversation({ unreadCount: 1, updatedAt: 2_000 });
    onChatReceived({ spaceKey: key, conversation, message: makeMessage(conversation) });
    const conv = getConversation(key, conversation.id);
    expect(conv?.unreadCount).toBe(1);
    expect(conv?.updatedAt).toBe(2_000);
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

  it('活跃会话：清零未读并回读 markConversationRead，updatedAt 仍取快照', () => {
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
});

// ------------------------------------------------------------------
// 数据面通路（fake SDK 后端 = 同一消息数据的另一读取方）
// ------------------------------------------------------------------

function fakeCtx(): PluginContext {
  return {
    pluginId: 'spark-chat',
    viewId: 'default',
    domain: 'plugin:spark-chat',
    space: { type: 'personal', id: 'personal' },
    theme: 'light',
    mount: { viewType: 'app' }
  };
}

/** 内存后端：fake sdk.messages 背后持有的「内核数据」（验证换前端数据原样在） */
function fakeBackend() {
  const conversations: Conversation[] = [];
  const messages: Record<string, ChatMessage[]> = {};
  const sent: Array<{ convId: string; text: string; messageId?: string }> = [];
  const api = {
    conversations: () => Promise.resolve(conversations.map((c) => ({ ...c }))),
    list: (convId: string) => Promise.resolve((messages[convId] ?? []).map((m) => ({ ...m }))),
    send: (convId: string, text: string, _quote?: unknown, messageId?: string) => {
      sent.push({ convId, text, messageId });
      const dto: ChatMessage = {
        id: messageId ?? `kernel-${sent.length}`,
        senderId: 'me',
        senderName: '我',
        type: 'text',
        content: text,
        createdAt: Date.now(),
        status: 'sent',
        recalled: false
      };
      (messages[convId] ??= []).push(dto);
      return Promise.resolve({ ...dto });
    },
    markConversationRead: () => Promise.resolve({ success: true }),
    onNewMessage: () => Promise.resolve(),
    onStatus: () => Promise.resolve(),
    onConversationsSynced: () => Promise.resolve(),
    onPeerPresence: () => Promise.resolve()
  } as unknown as PluginMessagesAPI;
  return { conversations, messages, sent, api };
}

describe('数据面经 sdk.messages 通路', () => {
  it('首次访问水合会话列表（conversations），写操作直通内核', async () => {
    const backend = fakeBackend();
    const seeded = makeConversation({ id: 'dm:peer-h1', peerId: 'peer-h1', unreadCount: 3, updatedAt: 9_000 });
    backend.conversations.push(seeded);
    bindPluginRuntime({ messages: backend.api } as unknown as PluginSDK, fakeCtx());

    const key = 'personal';
    // 同步返回缓存（空），异步水合后响应式刷新
    expect(listConversations(key)).toEqual([]);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const list = listConversations(key);
    expect(list.map((c) => c.id)).toEqual(['dm:peer-h1']);
    expect(list[0].unreadCount).toBe(3);
    expect(unreadCountOf(key)).toBe(3);
    markRead(key, 'dm:peer-h1');
    expect(getConversation(key, 'dm:peer-h1')?.unreadCount).toBe(0);
  });

  it('发送：乐观入列（sending）→ 内核回写状态；自生成 id 透传，水合按 id 合并不重复', async () => {
    const backend = fakeBackend();
    backend.conversations.push(makeConversation({ id: 'dm:peer-s1', peerId: 'peer-s1' }));
    bindPluginRuntime({ messages: backend.api } as unknown as PluginSDK, fakeCtx());
    // 模块级缓存跨用例共享：换绑定后端后清缓存触发重新水合（等价登录态切换语义）
    resetMessagesCache();

    const key = 'personal';
    // 首次访问触发水合，等水合落缓存后再发送
    listConversations(key);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const local = sendText(key, 'dm:peer-s1', '第一条');
    expect(local?.status).toBe('sending');
    expect(getMessages(key, 'dm:peer-s1')).toHaveLength(1);
    await new Promise((resolve) => setTimeout(resolve, 0));
    // 内核回写：状态翻转 + 自生成 id 透传到内核（与壳层 sendText 等语义）
    expect(getMessages(key, 'dm:peer-s1')[0].status).toBe('sent');
    expect(backend.sent).toHaveLength(1);
    expect(backend.sent[0].messageId).toBe(local?.id);
    // 另一读取方（换前端场景的最小聊天插件视角）经同一 sdk.messages 面读到同一条数据
    const reread = await backend.api.list('dm:peer-s1');
    expect(reread.map((m) => m.id)).toEqual([local?.id]);
  });
});
