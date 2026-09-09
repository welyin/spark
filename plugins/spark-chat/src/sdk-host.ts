/**
 * spark-chat 数据面适配层：把壳层 `stores/messages.ts` 依赖的宿主接口
 * （`messagesApi()` / `systemApi()` / `listenP2pEvents()` / `isTauri()`）
 * 以 **同签名** 桥接到插件 SDK（A18/A19 sdk.messages），store 主体零改动
 * （功能对等迁移的关键：同一组件树，只换数据源）。
 *
 * space 由桥绑定（插件实例按空间创建，壳层切换空间时重建实例并注入新
 * PluginContext）——适配层接收的 spaceKey 形参与绑定值一致时透传，
 * 不一致按绑定值执行（与桥「插件自报一律忽略」同口径）。
 *
 * v1 边界（communication §4.2 壳层保留面，插件内不实现）：
 * - 应用会话（`app:` 前缀）：会话列表适配层过滤；壳层应用会话挂载区呈现；
 * - 系统徽标（systemApi）：壳层职责，插件内返回 undefined。
 */
import type { PluginContext, PluginDataAPI, PluginSDK } from '../../../packages/plugin-sdk/src';
import type { HostMessagesApi, HostSystemApi } from './host-types';

// store 的类型来源（type-only，构建期擦除；本地同形拷贝，见 host-types.ts 头注）
export type { HostMessagesApi, HostSystemApi } from './host-types';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定运行上下文（index.ts 握手成功后调用） */
export function bindPluginRuntime(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 当前绑定空间（桥 ready 下发的权威值） */
export function pluginSpace(): PluginContext['space'] {
  return _ctx?.space ?? { type: 'personal', id: 'personal' };
}

/** 空间 key（`personal` / `org:{orgId}`），与壳层 spaceKeyOf 同口径 */
export function spaceKeyOf(space: PluginContext['space'] = pluginSpace()): string {
  return space.type === 'org' ? `org:${space.id}` : 'personal';
}

/** 当前绑定 spaceKey（store/组件的主键入参） */
export function boundSpaceKey(): string {
  return spaceKeyOf();
}

/** 桥上下文恒有 SDK（适配 store 的 isTauri 守卫——插件内恒为真） */
export function isTauri(): boolean {
  return true;
}

type MessagesApi = HostMessagesApi;

/** 应用会话 id 前缀（§20.1）：v1 由壳层挂载区呈现，插件列表过滤 */
const APP_CONV_PREFIX = 'app:';

/**
 * 内核消息接口的 SDK 适配（HostMessagesApi 同签名，与壳层 ElectronAPI['messages']
 * 等语义）：每个方法忽略 spaceKey 实参（space 已由桥绑定）；等语义纪律下实现即
 * A18/A19 SDK 调用直通，无任何行为加工。
 */
export function messagesApi(): MessagesApi | undefined {
  // 未绑定 SDK（vitest / 纯前端预览）退化为纯内存模式（与壳层 isTauri 守卫同口径）
  const sdk = _sdk;
  if (!sdk?.messages) return undefined;
  const m = sdk.messages;
  return {
    listConversations: (_spaceKey: string) =>
      // v1：应用会话由壳层挂载区呈现，插件列表过滤（communication §4.2）
      m.conversations().then((list) => list.filter((c) => !c.id.startsWith(APP_CONV_PREFIX))) as ReturnType<MessagesApi['listConversations']>,
    listMessages: (_spaceKey: string, convId: string) => m.list(convId) as ReturnType<MessagesApi['listMessages']>,
    ensureDirect: (_spaceKey: string, peerId: string, title: string) =>
      m.ensureDirect(peerId, title) as ReturnType<MessagesApi['ensureDirect']>,
    sendText: (_spaceKey: string, convId: string, messageId: string, text: string, quote?: Parameters<MessagesApi['sendText']>[4]) =>
      // 乐观入列的自生成 id 透传（与壳层 sendText 等语义：同 id 落库，水合按 id 合并不重复）
      m.send(convId, text, quote as Parameters<typeof m.send>[2], messageId) as ReturnType<MessagesApi['sendText']>,
    resend: (_spaceKey: string, convId: string, messageId: string) =>
      m.resend(convId, messageId) as ReturnType<MessagesApi['resend']>,
    recall: (_spaceKey: string, convId: string, messageId: string) => m.recall(convId, messageId),
    deleteMessage: (_spaceKey: string, convId: string, messageId: string) => m.deleteMessage(convId, messageId),
    markRead: (_spaceKey: string, convId: string) => m.markConversationRead(convId),
    setDraft: (_spaceKey: string, convId: string, draft: string) => m.setDraft(convId, draft),
    togglePin: (_spaceKey: string, convId: string) => m.togglePin(convId),
    toggleMute: (_spaceKey: string, convId: string) => m.toggleMute(convId),
    clear: (_spaceKey: string, convId: string) => m.clear(convId),
    deleteConversation: (_spaceKey: string, convId: string) => m.deleteConversation(convId),
    // 应用消息（服务号）：v1 不在插件内呈现（壳层挂载区），桩实现
    appSend: () => Promise.reject(new Error('app conversations are rendered by the shell mount area')),
    appList: async () => [],
    appMarkRead: async () => ({ success: true }),
    appDeleteConversation: async () => ({ success: true })
  } as MessagesApi;
}

/** 插件数据域 API（communication §4.3 过滤规则持久化面；未绑定 SDK 为 undefined → 纯内存模式） */
export function dataApi(): PluginDataAPI | undefined {
  return _sdk?.data;
}

type SystemApi = HostSystemApi;

/** 系统桥接（徽标等）：壳层职责，插件内恒不可用（watch 的 ?. 链静默跳过） */
export function systemApi(): SystemApi | undefined {
  return undefined;
}

/** 壳层 P2pEventDto 的最小同形（store 按 kind 判别联合消费；data 形状随 kind 定） */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type PluginP2pEvent = { kind: string; data: any };

let eventsBound = false;

/**
 * 事件订阅适配：壳层 `listenP2pEvents(handler)` → SDK 事件面。
 * 事件 data 注入 spaceKey（store 的 onChatReceived/onChatStatus 按其路由）。
 * 幂等（多次调用只订阅一次，handler 进集合）。
 */
export function listenP2pEvents(handler: (event: PluginP2pEvent) => void): Promise<void> {
  const sdk = _sdk;
  if (!sdk) return Promise.resolve();
  const key = spaceKeyOf();
  if (!eventsBound) {
    eventsBound = true;
    void sdk.messages?.onNewMessage((e) => {
      handler({
        kind: 'ChatReceived',
        data: { spaceKey: key, conversation: e.conversation, message: e.message }
      });
    });
    void sdk.messages?.onStatus((e) => {
      handler({ kind: 'ChatStatus', data: { spaceKey: key, ...e } });
    });
    void sdk.messages?.onConversationsSynced(() => {
      handler({ kind: 'ConversationsSynced', data: {} });
    });
    void sdk.messages?.onPeerPresence((e) => {
      handler({ kind: e.kind, data: { peerId: e.peerId } });
    });
  }
  return Promise.resolve();
}
