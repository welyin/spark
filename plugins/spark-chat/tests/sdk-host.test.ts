/**
 * spark-chat sdk-host 适配层单测（communication §4.2 功能对等迁移的数据面）：
 * - HostMessagesApi（与壳层 ElectronAPI['messages'] 同签名）逐方法等语义
 *   映射到 sdk.messages（space 由桥绑定，spaceKey 实参忽略）；
 * - 应用会话（app: 前缀）v1 由壳层挂载区呈现，列表适配层过滤；
 * - sendText 透传乐观入列的自生成 messageId（与壳层 sendText 等语义）；
 * - 未绑定 SDK 时退化为纯内存模式（返回 undefined / 订阅 no-op）。
 */
import { describe, expect, it } from 'vitest';
import type { PluginContext, PluginMessagesAPI, PluginSDK } from '../../../packages/plugin-sdk/src';
import {
  bindPluginRuntime,
  boundSpaceKey,
  listenP2pEvents,
  messagesApi,
  systemApi
} from '../src/sdk-host';

function fakeCtx(space: PluginContext['space'] = { type: 'personal', id: 'personal' }): PluginContext {
  return {
    pluginId: 'spark-chat',
    viewId: 'default',
    domain: 'plugin:spark-chat',
    space,
    theme: 'light',
    mount: { viewType: 'app' }
  };
}

type Calls = Array<{ method: string; args: unknown[] }>;

/** 记录调用的 sdk.messages 桩（事件订阅只记录不触发） */
function fakeMessages(calls: Calls, overrides: Partial<PluginMessagesAPI> = {}): PluginMessagesAPI {
  const record =
    (method: string, result: unknown) =>
    (...args: unknown[]) => {
      calls.push({ method, args });
      return Promise.resolve(result);
    };
  return {
    conversations: record('conversations', []),
    list: record('list', []),
    send: record('send', { id: 'm1', status: 'sent' }),
    recall: record('recall', { success: true }),
    markConversationRead: record('markConversationRead', { success: true }),
    onNewMessage: record('onNewMessage', undefined),
    onStatus: record('onStatus', undefined),
    onConversationsSynced: record('onConversationsSynced', undefined),
    onPeerPresence: record('onPeerPresence', undefined),
    ensureDirect: record('ensureDirect', { id: 'dm:p1' }),
    resend: record('resend', { id: 'm1', status: 'sent' }),
    deleteMessage: record('deleteMessage', { success: true }),
    setDraft: record('setDraft', { success: true }),
    togglePin: record('togglePin', { success: true }),
    toggleMute: record('toggleMute', { success: true }),
    clear: record('clear', { success: true }),
    deleteConversation: record('deleteConversation', { success: true }),
    ...overrides
  } as PluginMessagesAPI;
}

function bind(messages: PluginMessagesAPI | undefined, space?: PluginContext['space']): void {
  bindPluginRuntime({ messages } as unknown as PluginSDK, fakeCtx(space));
}

describe('sdk-host 适配层（HostMessagesApi → sdk.messages 等语义映射）', () => {
  it('绑定后 spaceKey 取桥下发值（插件自报忽略，实参透传不影响绑定）', () => {
    bind(fakeMessages([]));
    expect(boundSpaceKey()).toBe('personal');
    bind(fakeMessages([]), { type: 'org', id: 'org-1' });
    expect(boundSpaceKey()).toBe('org:org-1');
  });

  it('读面：conversations/list 直通 sdk.messages（spaceKey 实参忽略）', async () => {
    const calls: Calls = [];
    bind(fakeMessages(calls));
    const api = messagesApi();
    expect(api).toBeDefined();
    await api!.listConversations('whatever-space');
    await api!.listMessages('whatever', 'dm:p1');
    expect(calls.map((c) => c.method)).toEqual(['conversations', 'list']);
    expect(calls[1].args).toEqual(['dm:p1']);
  });

  it('应用会话（app: 前缀）在列表适配层过滤（壳层挂载区职责）', async () => {
    const calls: Calls = [];
    bind(
      fakeMessages(calls, {
        conversations: () =>
          Promise.resolve([
            { id: 'dm:p1', kind: 'direct' },
            { id: 'app:spark-moments', kind: 'app' },
            { id: 'dm:p2', kind: 'direct' }
          ] as never)
      })
    );
    const list = await messagesApi()!.listConversations('personal');
    expect(list.map((c) => c.id)).toEqual(['dm:p1', 'dm:p2']);
  });

  it('写面逐方法直通（ensureDirect/resend/recall/deleteMessage/markRead/setDraft/togglePin/toggleMute/clear/deleteConversation）', async () => {
    const calls: Calls = [];
    bind(fakeMessages(calls));
    const api = messagesApi()!;
    await api.ensureDirect('personal', 'p1', '对方');
    await api.resend('personal', 'dm:p1', 'm1');
    await api.recall('personal', 'dm:p1', 'm1');
    await api.deleteMessage('personal', 'dm:p1', 'm1');
    await api.markRead('personal', 'dm:p1');
    await api.setDraft('personal', 'dm:p1', '草稿');
    await api.togglePin('personal', 'dm:p1');
    await api.toggleMute('personal', 'dm:p1');
    await api.clear('personal', 'dm:p1');
    await api.deleteConversation('personal', 'dm:p1');
    expect(calls.map((c) => c.method)).toEqual([
      'ensureDirect',
      'resend',
      'recall',
      'deleteMessage',
      'markConversationRead',
      'setDraft',
      'togglePin',
      'toggleMute',
      'clear',
      'deleteConversation'
    ]);
    expect(calls[0].args).toEqual(['p1', '对方']);
    expect(calls[5].args).toEqual(['dm:p1', '草稿']);
  });

  it('sendText 透传乐观入列的自生成 messageId（与壳层 sendText 等语义）', async () => {
    const calls: Calls = [];
    bind(fakeMessages(calls));
    await messagesApi()!.sendText('personal', 'dm:p1', 'm1700-1', '你好', {
      messageId: 'm0',
      senderName: '对方',
      preview: '原消息'
    });
    expect(calls).toHaveLength(1);
    expect(calls[0].method).toBe('send');
    // sdk.messages.send(convId, text, quote, messageId)
    expect(calls[0].args).toEqual(['dm:p1', '你好', { messageId: 'm0', senderName: '对方', preview: '原消息' }, 'm1700-1']);
  });

  it('应用消息面 v1 为桩（appList 恒空、appSend 拒绝），systemApi 恒不可用', async () => {
    bind(fakeMessages([]));
    const api = messagesApi()!;
    await expect(api.appList('personal', 'spark-moments')).resolves.toEqual([]);
    await expect(api.appSend('personal', 'spark-moments', { summary: 'x' })).rejects.toThrow(/shell mount area/);
    expect(systemApi()).toBeUndefined();
  });

  it('事件订阅经桥事件面：ChatReceived/ChatStatus/Presence 事件形状与壳层 P2pEventDto 同口径', async () => {
    const calls: Calls = [];
    const handlers: Record<string, (e: never) => void> = {};
    bind(
      fakeMessages(calls, {
        onNewMessage: ((h: (e: unknown) => void) => {
          handlers.newMessage = h as never;
          return Promise.resolve();
        }) as never,
        onStatus: ((h: (e: unknown) => void) => {
          handlers.status = h as never;
          return Promise.resolve();
        }) as never,
        onPeerPresence: ((h: (e: unknown) => void) => {
          handlers.presence = h as never;
          return Promise.resolve();
        }) as never
      }),
      { type: 'org', id: 'org-9' }
    );
    const received: Array<{ kind: string; data: Record<string, unknown> }> = [];
    await listenP2pEvents((event) => received.push(event));
    handlers.newMessage({ conversation: { id: 'dm:p1' }, message: { id: 'm1' } } as never);
    handlers.status({ convId: 'dm:p1', messageId: 'm1', status: 'read' } as never);
    handlers.presence({ kind: 'PeerConnected', peerId: 'p1' } as never);
    expect(received).toEqual([
      { kind: 'ChatReceived', data: { spaceKey: 'org:org-9', conversation: { id: 'dm:p1' }, message: { id: 'm1' } } },
      { kind: 'ChatStatus', data: { spaceKey: 'org:org-9', convId: 'dm:p1', messageId: 'm1', status: 'read' } },
      { kind: 'PeerConnected', data: { peerId: 'p1' } }
    ]);
  });

  it('未绑定 SDK：messagesApi 返回 undefined（纯内存模式），listenP2pEvents no-op', async () => {
    bindPluginRuntime(null as unknown as PluginSDK, fakeCtx());
    expect(messagesApi()).toBeUndefined();
    await expect(listenP2pEvents(() => {})).resolves.toBeUndefined();
  });
});
