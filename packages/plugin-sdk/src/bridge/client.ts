/**
 * 插件侧桥客户端：connectPluginBridge。
 *
 * 插件 bundle 在 iframe 内加载后调用本函数完成握手，拿到与现有 PluginSDK
 * 签名完全一致的 SDK 实例——每个方法序列化为 call 消息经 window.parent
 * postMessage 发给宿主，Promise 化等待 result；事件模块（sdk.events）
 * 经 subscribe/unsubscribe 消息注册，宿主推送 event 时分发给本地回调；
 * 宿主心跳 ping 由本客户端自动应答 pong（熔断配套）；运行时错误
 * （onerror/unhandledrejection）自动经 event `runtime-error` 上行上报，
 * 同文案 10s 去重、总量封顶，供宿主熔断计数。
 */

import type {
  PluginContext,
  PluginCollectionSchema,
  PluginDeclaredCollectionSchema,
  PluginDocQueryOptions,
  PluginEventHandler,
  PluginSDK
} from '../index';
import {
  BRIDGE_PROTOCOL_VERSION,
  parseBridgeMessage,
  type BridgeResultMessage
} from './protocol';

/** 消息端点的最小接口（Window 的子集，便于测试注入伪造端点） */
export type WindowMessageEndpoint = Pick<Window, 'addEventListener' | 'removeEventListener' | 'postMessage'>;

export type ConnectPluginBridgeOptions = {
  pluginId: string;
  viewId: string;
  /** 插件依赖的 SDK 契约版本（应与 manifest.sdkVersion 一致），默认 '1' */
  sdkVersion?: string;
  /** 单次 call 超时（默认 10s） */
  callTimeoutMs?: number;
  /** 握手超时（默认 10s） */
  handshakeTimeoutMs?: number;
  /** postMessage targetOrigin（默认 '*'；宿主 origin 确定时建议显式传入） */
  targetOrigin?: string;
  /** 监听消息的窗口（默认 window，即插件自身 iframe 窗口） */
  listenWindow?: WindowMessageEndpoint;
  /** 宿主窗口（默认 window.parent） */
  hostWindow?: WindowMessageEndpoint;
};

export type PluginBridgeConnection = {
  sdk: PluginSDK;
  ctx: PluginContext;
};

type PendingRequest = {
  resolve: (data: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

/**
 * 连接宿主桥：发送 hello，等待 ready（或版本不兼容 error）后返回
 * 完整 PluginSDK 实例与运行上下文。
 *
 * @throws 握手超时、宿主回 error（如 SDK 版本不兼容）
 */
export function connectPluginBridge(options: ConnectPluginBridgeOptions): Promise<PluginBridgeConnection> {
  const listenWindow = options.listenWindow ?? window;
  const hostWindow = options.hostWindow ?? window.parent;
  const targetOrigin = options.targetOrigin ?? '*';
  const callTimeoutMs = options.callTimeoutMs ?? 10_000;
  const handshakeTimeoutMs = options.handshakeTimeoutMs ?? 10_000;

  let counter = 0;
  const pending = new Map<string, PendingRequest>();
  const eventHandlers = new Map<string, Set<PluginEventHandler>>();

  const nextId = (prefix: string): string => `${prefix}-${Date.now().toString(36)}-${++counter}`;

  const post = (message: unknown): void => {
    hostWindow.postMessage(message, targetOrigin);
  };

  // ------------------------------------------------------------------
  // 运行时错误自动上报（熔断配套，设计文档「熔断与治理」异常上报桥）：
  // onerror / unhandledrejection 经桥 event `runtime-error` 上行给宿主计数。
  // 防刷屏：同文案 10s 内去重，总量封顶 50 条。
  // ------------------------------------------------------------------
  const reportedErrors = new Map<string, number>();
  const MAX_REPORTED_ERRORS = 50;
  const ERROR_DEDUPE_WINDOW_MS = 10_000;

  const reportRuntimeError = (message: string): void => {
    if (!message) {
      return;
    }
    const now = Date.now();
    const last = reportedErrors.get(message);
    if (last !== undefined && now - last < ERROR_DEDUPE_WINDOW_MS) {
      return;
    }
    if (reportedErrors.size >= MAX_REPORTED_ERRORS) {
      return;
    }
    reportedErrors.set(message, now);
    post({ v: BRIDGE_PROTOCOL_VERSION, type: 'event', event: 'runtime-error', payload: { message } });
  };

  /** 非 Error rejection 的稳妥字符串化（裸 String(obj) 只得 '[object Object]'） */
  const stringifyRejectionReason = (reason: unknown): string => {
    if (reason instanceof Error) {
      return reason.message;
    }
    if (typeof reason === 'string') {
      return reason;
    }
    try {
      return JSON.stringify(reason) ?? String(reason);
    } catch {
      // 循环引用等序列化失败时兜底
      return String(reason);
    }
  };

  const onRuntimeError = ((event: ErrorEvent) => {
    reportRuntimeError(event.message || 'unknown error');
  }) as EventListener;
  const onUnhandledRejection = ((event: PromiseRejectionEvent) => {
    reportRuntimeError(stringifyRejectionReason(event.reason));
  }) as EventListener;
  listenWindow.addEventListener('error', onRuntimeError);
  listenWindow.addEventListener('unhandledrejection', onUnhandledRejection);

  /** 握手失败（超时/被拒）后摘除错误上报监听：桥已不可用，留着只会空报 */
  const removeErrorListeners = (): void => {
    listenWindow.removeEventListener('error', onRuntimeError);
    listenWindow.removeEventListener('unhandledrejection', onUnhandledRejection);
  };

  /** 发送一条期待 result 应答的消息（call/subscribe/unsubscribe），Promise 化等待 */
  const request = (message: { id: string } & Record<string, unknown>, timeoutMs: number): Promise<unknown> =>
    new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(message.id);
        reject(new Error(`Plugin bridge request timed out after ${timeoutMs}ms (${String(message.type)})`));
      }, timeoutMs);
      pending.set(message.id, { resolve, reject, timer });
      post(message);
    });

  const call = (module: string, method: string, args: unknown[]): Promise<unknown> =>
    request(
      { v: BRIDGE_PROTOCOL_VERSION, type: 'call', id: nextId('call'), module, method, args },
      callTimeoutMs
    );

  const settleResult = (message: BridgeResultMessage): void => {
    const entry = pending.get(message.id);
    if (!entry) {
      return;
    }
    pending.delete(message.id);
    clearTimeout(entry.timer);
    if (message.ok) {
      entry.resolve(message.data);
    } else {
      const code = message.error?.code ?? 'call-failed';
      const text = message.error?.message ?? 'Plugin bridge call failed';
      entry.reject(Object.assign(new Error(text), { code }));
    }
  };

  const dispatchEvent = (event: string, payload: unknown): void => {
    const handlers = eventHandlers.get(event);
    if (!handlers) {
      return;
    }
    for (const handler of handlers) {
      try {
        handler(payload);
      } catch (error) {
        console.error(`[plugin-bridge] 事件回调抛错（${event}）：`, error);
      }
    }
  };

  return new Promise<PluginBridgeConnection>((resolveHandshake, rejectHandshake) => {
    let handshakeSettled = false;
    const handshakeTimer = setTimeout(() => {
      if (handshakeSettled) {
        return;
      }
      handshakeSettled = true;
      listenWindow.removeEventListener('message', onMessage as EventListener);
      removeErrorListeners();
      rejectHandshake(new Error(`Plugin bridge handshake timed out after ${handshakeTimeoutMs}ms`));
    }, handshakeTimeoutMs);

    const onMessage = (event: MessageEvent): void => {
      // 只接受来自宿主窗口的消息（伪造 source 的一律丢弃）
      if (event.source !== (hostWindow as unknown)) {
        return;
      }
      const message = parseBridgeMessage(event.data);
      if (!message) {
        return;
      }

      // 心跳自动应答（熔断配套，与握手状态无关）
      if (message.type === 'ping') {
        post({ v: BRIDGE_PROTOCOL_VERSION, type: 'pong', id: message.id });
        return;
      }

      if (!handshakeSettled) {
        if (message.type === 'ready') {
          handshakeSettled = true;
          clearTimeout(handshakeTimer);
          resolveHandshake({ sdk: createBridgeSdk(message.ctx), ctx: message.ctx });
        } else if (message.type === 'error') {
          handshakeSettled = true;
          clearTimeout(handshakeTimer);
          listenWindow.removeEventListener('message', onMessage as EventListener);
          removeErrorListeners();
          rejectHandshake(
            Object.assign(new Error(`Plugin bridge handshake rejected: ${message.message}`), { code: message.code })
          );
        }
        return;
      }

      if (message.type === 'result') {
        settleResult(message);
      } else if (message.type === 'event') {
        dispatchEvent(message.event, message.payload);
      }
      // action（卡片回调预留）：本波不路由，直接忽略
    };

    const createBridgeSdk = (ctx: PluginContext): PluginSDK => ({
      domain: ctx.domain,
      evidence: {
        headHash: () => call('evidence', 'headHash', []) as Promise<{ hash: string | null }>,
        verify: () => call('evidence', 'verify', []) as Promise<{ valid: boolean; height: number }>
      },
      p2p: {
        start: () => call('p2p', 'start', []) as Promise<{ started: boolean }>,
        stop: () => call('p2p', 'stop', []) as Promise<{ started: boolean }>,
        broadcast: (topic, message) =>
          call('p2p', 'broadcast', [topic, message]) as Promise<{ success: boolean }>
      },
      runtime: {
        currentRoot: () =>
          call('runtime', 'currentRoot', []) as Promise<{ unlocked: boolean; rootId: string | null }>,
        syncOrganizationData: (orgId) =>
          call('runtime', 'syncOrganizationData', [orgId]) as Promise<{
            orgId: string;
            attempted: number;
            pulled: number;
          }>,
        listMineOrganizations: () => call('runtime', 'listMineOrganizations', []) as ReturnType<PluginSDK['runtime']['listMineOrganizations']>
      },
      docs: {
        get: <T extends Record<string, unknown> = Record<string, unknown>>(collection: string, id: string) =>
          call('docs', 'get', [collection, id]) as Promise<T | null>,
        defineCollection: (collection: string, schema: PluginCollectionSchema) =>
          call('docs', 'defineCollection', [collection, schema]) as Promise<PluginDeclaredCollectionSchema>,
        put: (collection: string, id: string, doc: Record<string, unknown>) =>
          call('docs', 'put', [collection, id, doc]) as Promise<{ success: boolean }>,
        delete: (collection: string, id: string) =>
          call('docs', 'delete', [collection, id]) as Promise<{ success: boolean }>,
        query: <T extends Record<string, unknown> = Record<string, unknown>>(
          collection: string,
          queryOptions: PluginDocQueryOptions = {}
        ) =>
          call('docs', 'query', [collection, queryOptions]) as Promise<{
            items: Array<{ id: string; data: T }>;
            nextCursor?: string;
          }>
      },
      identity: {
        sign: (payload) =>
          call('identity', 'sign', [payload]) as ReturnType<PluginSDK['identity']['sign']>,
        verify: (payload, signature, publicKey) =>
          call('identity', 'verify', [payload, signature, publicKey]) as Promise<{ valid: boolean }>
      },
      events: {
        subscribe: async (event, handler) => {
          await request(
            { v: BRIDGE_PROTOCOL_VERSION, type: 'subscribe', id: nextId('sub'), event },
            callTimeoutMs
          );
          const handlers = eventHandlers.get(event) ?? new Set<PluginEventHandler>();
          handlers.add(handler);
          eventHandlers.set(event, handlers);
        },
        unsubscribe: async (event, handler) => {
          await request(
            { v: BRIDGE_PROTOCOL_VERSION, type: 'unsubscribe', id: nextId('unsub'), event },
            callTimeoutMs
          );
          const handlers = eventHandlers.get(event);
          if (!handlers) {
            return;
          }
          if (handler) {
            handlers.delete(handler);
          } else {
            handlers.clear();
          }
          if (handlers.size === 0) {
            eventHandlers.delete(event);
          }
        }
      }
    });

    listenWindow.addEventListener('message', onMessage as EventListener);
    post({
      v: BRIDGE_PROTOCOL_VERSION,
      type: 'hello',
      sdkVersion: options.sdkVersion ?? '1',
      pluginId: options.pluginId,
      viewId: options.viewId
    });
  });
}
