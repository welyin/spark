/**
 * A19 验收（communication §六）：第三方「最小聊天插件」安装后与默认内置
 * spark-chat 读写同一消息数据（换前端数据原样在）。
 *
 * 结构：同一份内核消息后端桩（electronAPI.messages 壳层命令面 = sled 数据
 * 的壳层投影）上挂两个桥 dispatcher——spark-minichat（第三方示例）与
 * spark-chat（默认内置，经其 sdk-host 适配层 + 消息 store）。双向断言：
 * minichat 经桥发的消息 spark-chat store 水合可见；spark-chat 发的消息
 * minichat 经桥 list 可见——同一数据，零迁移。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('element-plus', async (importOriginal) => {
  const actual = await importOriginal<typeof import('element-plus')>();
  return { ...actual, ElMessageBox: { confirm: vi.fn() } };
});

import { createPluginBridgeDispatcher } from '../../plugin/bridge-dispatcher';
import { bindPluginRuntime } from '../../../../plugins/spark-chat/src/sdk-host';
import {
  getMessages,
  listConversations,
  resetMessagesCache,
  sendText
} from '../../../../plugins/spark-chat/src/store';
import type { PluginContext, PluginSDK } from '../../../../packages/plugin-sdk/src';
import type { ConversationDto, ChatMessageDto } from '../../../../plugins/spark-chat/src/host-types';

/** 内核消息后端桩（壳层 ElectronAPI['messages'] 面；两插件共享 = 同一 sled 数据） */
function makeKernelBackend() {
  const conversations: ConversationDto[] = [
    {
      id: 'dm:peer-a',
      kind: 'direct',
      title: '对方A',
      peerId: 'peer-a',
      unreadCount: 0,
      pinnedAt: 0,
      muted: false,
      online: false,
      draft: '',
      updatedAt: 1_000
    }
  ];
  const messages: Record<string, ChatMessageDto[]> = {};
  let seq = 0;
  const api = {
    listConversations: async (_spaceKey: string) => conversations.map((c) => ({ ...c })),
    listMessages: async (_spaceKey: string, convId: string) => (messages[convId] ?? []).map((m) => ({ ...m })),
    sendText: async (_spaceKey: string, convId: string, messageId: string, text: string) => {
      const dto: ChatMessageDto = {
        id: messageId || `kernel-${++seq}`,
        senderId: 'me',
        senderName: '我',
        type: 'text',
        content: text,
        createdAt: Date.now(),
        status: 'sent',
        recalled: false
      };
      (messages[convId] ??= []).push(dto);
      return { ...dto };
    },
    ensureDirect: async () => conversations[0],
    resend: async () => ({}),
    recall: async () => ({ success: true }),
    deleteMessage: async () => ({ success: true }),
    markRead: async () => ({ success: true }),
    setDraft: async () => ({ success: true }),
    togglePin: async () => ({ success: true }),
    toggleMute: async () => ({ success: true }),
    clear: async () => ({ success: true }),
    deleteConversation: async () => ({ success: true })
  };
  return { api, conversations, messages };
}

/** 后端桩：任意层级任意方法返回 Promise<null>（非本案调用面兜底） */
const makeNullApi = (): any =>
  new Proxy(function () {}, {
    get(target, key) {
      return key in target ? (target as any)[key] : makeNullApi();
    },
    apply() {
      return Promise.resolve(null);
    }
  });

const PERSONAL = { type: 'personal', id: 'personal' } as const;

function identityFor(pluginId: string) {
  return {
    pluginId,
    viewId: 'default',
    domain: `plugin:${pluginId}`,
    space: PERSONAL,
    pluginName: pluginId,
    supportedSpaces: ['personal', 'org'] as Array<'personal' | 'org'>
  };
}

/** 经桥 dispatcher 构造 spark-chat 的 SDK 句柄（桥 client 同形：call(module, method, args)） */
function sdkOverDispatcher(handler: (module: string, method: string, args: unknown[]) => Promise<unknown>): PluginSDK {
  return {
    messages: {
      conversations: () => handler('messages', 'conversations', []),
      list: (convId: string) => handler('messages', 'list', [convId]),
      send: (convId: string, text: string, quote?: unknown, messageId?: string) =>
        handler('messages', 'send', [convId, text, quote ?? null, messageId ?? null]),
      markConversationRead: (convId: string) => handler('messages', 'markConversationRead', [convId]),
      onNewMessage: async () => {},
      onStatus: async () => {},
      onConversationsSynced: async () => {},
      onPeerPresence: async () => {}
    }
  } as unknown as PluginSDK;
}

function chatCtx(): PluginContext {
  return {
    pluginId: 'spark-chat',
    viewId: 'default',
    domain: 'plugin:spark-chat',
    space: { ...PERSONAL },
    theme: 'light',
    mount: { viewType: 'app' }
  };
}

let backend: ReturnType<typeof makeKernelBackend>;

beforeEach(() => {
  vi.clearAllMocks();
  backend = makeKernelBackend();
  (window.electronAPI as any).messages = backend.api;
  (window.electronAPI as any).plugin = makeNullApi();
  (window.electronAPI as any).contacts = makeNullApi();
  (window.electronAPI as any).feed = makeNullApi();
  // 市场安装状态：两插件均已安装并授予 messages:read/write（高危确认已在安装时完成）
  (window.electronAPI as any).pluginMarket = {
    list: async () => [
      { id: 'spark-chat', grantedPermissions: ['messages:read', 'messages:write'] },
      { id: 'spark-minichat', grantedPermissions: ['messages:read', 'messages:write'] }
    ]
  };
  resetMessagesCache();
});

describe('A19 验收：最小聊天插件与默认内置插件读写同一消息数据', () => {
  it('minichat 经桥发送 → spark-chat store 水合可见（同 id 同内容）', async () => {
    // 第三方最小聊天插件：桥四个调用（conversations/list/send/onNewMessage）
    const minichat = await createPluginBridgeDispatcher(identityFor('spark-minichat'));
    const convs = (await minichat('messages', 'conversations', [])) as ConversationDto[];
    expect(convs.map((c) => c.id)).toEqual(['dm:peer-a']);
    await minichat('messages', 'send', ['dm:peer-a', '来自最小聊天插件', null, 'mini-m1']);

    // 默认内置 spark-chat：经自身 sdk-host 适配层 + store 读同一数据
    const chatDispatcher = await createPluginBridgeDispatcher(identityFor('spark-chat'));
    bindPluginRuntime(sdkOverDispatcher(chatDispatcher), chatCtx());
    resetMessagesCache();
    listConversations('personal');
    await new Promise((resolve) => setTimeout(resolve, 0));

    // 首次访问触发消息水合（异步），等水合落缓存后再断言
    getMessages('personal', 'dm:peer-a');
    await new Promise((resolve) => setTimeout(resolve, 0));
    const stored = getMessages('personal', 'dm:peer-a');
    expect(stored.map((m) => m.id)).toEqual(['mini-m1']);
    expect(stored[0].content).toBe('来自最小聊天插件');
  });

  it('spark-chat store 发送 → minichat 经桥 list 可见（换前端数据原样在）', async () => {
    const chatDispatcher = await createPluginBridgeDispatcher(identityFor('spark-chat'));
    bindPluginRuntime(sdkOverDispatcher(chatDispatcher), chatCtx());
    resetMessagesCache();
    listConversations('personal');
    await new Promise((resolve) => setTimeout(resolve, 0));

    const local = sendText('personal', 'dm:peer-a', '来自默认内置聊天插件');
    expect(local).toBeDefined();
    await new Promise((resolve) => setTimeout(resolve, 0));

    // 第三方插件重读：乐观入列的自生成 id 随发送透传落库，按 id 可见同一条
    const minichat = await createPluginBridgeDispatcher(identityFor('spark-minichat'));
    const reread = (await minichat('messages', 'list', ['dm:peer-a'])) as ChatMessageDto[];
    expect(reread.map((m) => m.id)).toEqual([local?.id]);
    expect(reread[0].content).toBe('来自默认内置聊天插件');
  });

  it('未授权插件（未声明 messages:read）读会话被拒（权限位三重过滤回归）', async () => {
    (window.electronAPI as any).pluginMarket = {
      list: async () => [{ id: 'evil-chat', grantedPermissions: [] }]
    };
    const evil = await createPluginBridgeDispatcher(identityFor('evil-chat'));
    await expect(evil('messages', 'conversations', [])).rejects.toThrow(/Access denied: permission "messages:read"/);
    await expect(evil('messages', 'send', ['dm:peer-a', 'x', null, null])).rejects.toThrow(/Access denied: permission "messages:write"/);
  });
});
