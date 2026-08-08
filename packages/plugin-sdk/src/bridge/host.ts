/**
 * 宿主侧桥调度器：createBridgeHost。
 *
 * 壳层为每个插件 iframe 创建一个桥宿主：监听 message 事件，严格校验
 * event.origin 与 event.source（只接受对应 iframe 的消息），完成握手后把
 * call 分发给 handler（由壳层接到 invoke 适配层，下一波接线）；支持向插件
 * 推 event（仅在该事件已被订阅时发送）与发 ping（心跳熔断配套）。
 *
 * 本波只提供通用机制，不接壳层业务。
 */

import type { PluginContext } from '../index';
import {
  BRIDGE_ERROR_CALL_FAILED,
  BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE,
  BRIDGE_PROTOCOL_VERSION,
  isSdkVersionCompatible,
  parseBridgeMessage
} from './protocol';
import type { WindowMessageEndpoint } from './client';

/** call 分发处理器：(模块, 方法, 参数) => 结果（壳层后续接到 invoke 适配层） */
export type BridgeHostHandler = (module: string, method: string, args: unknown[]) => Promise<unknown>;

/**
 * handler 供应方式：
 * - 直接传 BridgeHostHandler：同步可用；
 * - 传 () => Promise<BridgeHostHandler>：延迟解析（壳层的 dispatcher 需先经
 *   Tauri invoke 读授权清单，若同步等待会推迟 createBridgeHost 的 message 监听
 *   注册，导致插件 hello 发出时无人接收而握手超时）。懒解析保证监听立即注册，
 *   handler 在首个 call 到来时才解析（此刻握手已完成，hello 已被正确接收）。
 */
export type BridgeHostHandlerSource = BridgeHostHandler | (() => Promise<BridgeHostHandler>);

export type CreateBridgeHostOptions = {
  /** 插件 iframe（仅需 contentWindow，结构类型便于测试注入） */
  iframe: Pick<HTMLIFrameElement, 'contentWindow'>;
  pluginId: string;
  viewId: string;
  /** 期望的插件 iframe origin，严格校验（独立 origin；沙箱 opaque origin 为 'null'） */
  expectedOrigin: string;
  /** 宿主支持的 SDK 契约版本（默认 '1'），握手时与 manifest sdkVersion 协商 */
  sdkVersion?: string;
  /** 握手 ready 下发的插件运行上下文 */
  ctx: PluginContext;
  handler: BridgeHostHandlerSource;
  /** 握手超时（默认 10s；超时按一次加载异常计，设计文档「熔断与治理」） */
  handshakeTimeoutMs?: number;
  /** postMessage targetOrigin（默认取 expectedOrigin；沙箱 opaque origin 时必须显式传 '*'） */
  targetOrigin?: string;
  /** 监听消息的窗口（默认 window，即壳层窗口） */
  listenWindow?: WindowMessageEndpoint;
  /**
   * 插件→宿主 event 上行通道（熔断错误上报等，如 runtime-error）；
   * 与宿主→插件的订阅推送共用 event 信封，方向相反
   */
  onEvent?: (event: string, payload: unknown) => void;
  /**
   * 插件（message-card）→宿主 action 上行通道（卡片按钮回调）；
   * 宿主按桥绑定身份校验归属后路由给主视图实例（壳层 plugin-card-actions.ts）
   */
  onAction?: (cardId: string, actionId: string, data?: unknown) => void;
};

export type BridgeHost = {
  /** 握手完成（收到合法 hello、版本兼容、ready 已下发）；超时或版本不兼容 reject */
  ready: Promise<PluginContext>;
  /** 向插件推送事件（仅当插件已订阅该事件时才发送） */
  pushEvent: (event: string, payload?: unknown) => void;
  /** 向插件推送卡片按钮回调（主视图实例的 onCardAction 接收；无订阅语义，直接下发） */
  pushAction: (cardId: string, actionId: string, data?: unknown) => void;
  /** 心跳：发 ping 并等待 pong，超时 reject（默认 5s）；destroy 后立即 reject */
  ping: (timeoutMs?: number) => Promise<void>;
  /**
   * 宿主→插件反向调用：发 host-call 并等插件 host-result 应答。
   * 插件侧经 onHostCall(event, handler) 注册处理器；无处理器/超时/插件异常均 reject。
   * 用于宿主主动向插件查询（如删除联系人前询问「该 bot 是否还存在」）。
   */
  request: (event: string, payload?: unknown, timeoutMs?: number) => Promise<unknown>;
  /** 关闭桥：移除监听、清理定时器与待决请求；握手未 settle 时 ready 以 destroyed 拒绝 */
  destroy: () => void;
};

export function createBridgeHost(options: CreateBridgeHostOptions): BridgeHost {
  const listenWindow = options.listenWindow ?? window;
  const remote = options.iframe.contentWindow;
  const targetOrigin = options.targetOrigin ?? options.expectedOrigin;
  const hostSdkVersion = options.sdkVersion ?? '1';

  let handshakeSettled = false;
  let destroyed = false;
  let counter = 0;
  /** handler 懒解析缓存（首次 call 时解析，并发复用同一 Promise） */
  let resolvedHandler: BridgeHostHandler | undefined;
  let handlerPromise: Promise<BridgeHostHandler> | undefined;
  const resolveHandler = (): Promise<BridgeHostHandler> => {
    if (resolvedHandler) {
      return Promise.resolve(resolvedHandler);
    }
    if (!handlerPromise) {
      const source = options.handler;
      handlerPromise = (typeof source === 'function' && source.length === 0
        ? (source as () => Promise<BridgeHostHandler>)()
        : Promise.resolve(source as BridgeHostHandler)
      ).then((h) => {
        resolvedHandler = h;
        return h;
      });
    }
    return handlerPromise;
  };
  /** 插件已订阅的事件集合 */
  const subscriptions = new Set<string>();
  const pendingPings = new Map<string, { resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  /** 宿主→插件反向调用的待决表（id → resolve/reject/timer） */
  const pendingRequests = new Map<string, { resolve: (data: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  /** destroy 时移除握手监听/定时器（在 ready promise 执行器内赋值） */
  let readyCleanup: () => void = () => {};
  /** destroy 时 reject 未 settle 的握手（在 ready promise 执行器内赋值，执行器同步运行故必先于任何 destroy 调用） */
  let rejectReadyRef: (error: Error) => void = () => {};

  const post = (message: unknown): void => {
    if (destroyed) {
      return;
    }
    remote?.postMessage(message, targetOrigin);
  };

  const ready = new Promise<PluginContext>((resolveReady, rejectReady) => {
    const handshakeTimer = setTimeout(() => {
      if (handshakeSettled || destroyed) {
        return;
      }
      handshakeSettled = true;
      listenWindow.removeEventListener('message', onMessage as EventListener);
      rejectReady(new Error(`Plugin bridge handshake timed out after ${options.handshakeTimeoutMs ?? 10_000}ms`));
    }, options.handshakeTimeoutMs ?? 10_000);

    const settleHandshake = (settle: () => void): void => {
      if (handshakeSettled) {
        return;
      }
      handshakeSettled = true;
      clearTimeout(handshakeTimer);
      settle();
    };

    const onMessage = (event: MessageEvent): void => {
      if (destroyed) {
        return;
      }
      // 校验 origin：必须匹配本桥绑定的 iframe
      if (event.origin !== options.expectedOrigin) {
        return;
      }
      // source 校验：WebView2 下 sandbox iframe 的 event.source 可能是 proxy，
      // 与 contentWindow 引用不是同一个 JS 对象，严格 !== 会误判。
      // opaque origin ('null') 已提供充分隔离 → 此时不做 source 引用比较。
      if (event.origin !== 'null' && event.source !== (remote as unknown)) {
        return;
      }
      const message = parseBridgeMessage(event.data);
      if (!message) {
        return;
      }

      switch (message.type) {
        case 'hello': {
          // 插件身份以桥创建时绑定为准，hello 自报的 pluginId/viewId 只做一致性核对
          if (message.pluginId !== options.pluginId || message.viewId !== options.viewId) {
            settleHandshake(() => {
              post({
                v: BRIDGE_PROTOCOL_VERSION,
                type: 'error',
                code: 'identity-mismatch',
                message: `Plugin identity mismatch: expected ${options.pluginId}/${options.viewId}, got ${message.pluginId}/${message.viewId}`
              });
              rejectReady(new Error('Plugin bridge handshake failed: identity mismatch'));
            });
            return;
          }
          if (!isSdkVersionCompatible(hostSdkVersion, message.sdkVersion)) {
            settleHandshake(() => {
              post({
                v: BRIDGE_PROTOCOL_VERSION,
                type: 'error',
                code: BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE,
                message: `SDK version incompatible: host supports "${hostSdkVersion}", plugin requires "${message.sdkVersion}"`
              });
              rejectReady(
                Object.assign(new Error('Plugin bridge handshake failed: SDK version incompatible'), {
                  code: BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE
                })
              );
            });
            return;
          }
          settleHandshake(() => {
            post({ v: BRIDGE_PROTOCOL_VERSION, type: 'ready', sdkVersion: hostSdkVersion, ctx: options.ctx });
            resolveReady(options.ctx);
          });
          return;
        }
        case 'call': {
          // 握手完成前的调用直接失败，避免未授权窗口期
          if (!handshakeSettled) {
            post({
              v: BRIDGE_PROTOCOL_VERSION,
              type: 'result',
              id: message.id,
              ok: false,
              error: { code: 'not-ready', message: 'Plugin bridge handshake not completed' }
            });
            return;
          }
          resolveHandler()
            .then((handler) => handler(message.module, message.method, message.args))
            .then(
              (data) => post({ v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: message.id, ok: true, data }),
              (error: unknown) =>
                post({
                  v: BRIDGE_PROTOCOL_VERSION,
                  type: 'result',
                  id: message.id,
                  ok: false,
                  error: {
                    code: BRIDGE_ERROR_CALL_FAILED,
                    message: error instanceof Error ? error.message : String(error)
                  }
                })
            );
          return;
        }
        case 'subscribe':
          subscriptions.add(message.event);
          post({ v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: message.id, ok: true });
          return;
        case 'unsubscribe':
          subscriptions.delete(message.event);
          post({ v: BRIDGE_PROTOCOL_VERSION, type: 'result', id: message.id, ok: true });
          return;
        case 'pong': {
          const pending = pendingPings.get(message.id);
          if (pending) {
            pendingPings.delete(message.id);
            clearTimeout(pending.timer);
            pending.resolve();
          }
          return;
        }
        case 'host-result': {
          // 插件对宿主反向调用的应答（无握手门控——request 仅在握手后由壳层发起）
          const pending = pendingRequests.get(message.id);
          if (pending) {
            pendingRequests.delete(message.id);
            clearTimeout(pending.timer);
            if (message.ok) {
              pending.resolve(message.data);
            } else {
              pending.reject(new Error(message.error?.message ?? 'host-call failed'));
            }
          }
          return;
        }
        case 'event':
          // 插件→宿主上行事件（runtime-error 错误上报、card-resize 高度申请等）：
          // 与 call 同口径按握手门控，握手完成前一律丢弃（避免未授权窗口期）
          if (!handshakeSettled) {
            return;
          }
          options.onEvent?.(message.event, message.payload);
          return;
        case 'action':
          // 插件（message-card）→宿主卡片按钮回调上行，归属校验与路由在壳层；
          // 同样按握手门控，握手完成前一律丢弃
          if (!handshakeSettled) {
            return;
          }
          options.onAction?.(message.cardId, message.actionId, message.data);
          return;
        default:
          // 其余方向的消息（ready/result/ping）不应由插件发出，忽略
          return;
      }
    };

    listenWindow.addEventListener('message', onMessage as EventListener);

    // destroy 需要移除监听：把引用挂到外层变量透出（见下方 destroy 实现）
    readyCleanup = () => {
      clearTimeout(handshakeTimer);
      listenWindow.removeEventListener('message', onMessage as EventListener);
    };
    // destroy 需要 reject 未 settle 的握手（幂等：已 settle 则无操作）
    rejectReadyRef = (error: Error) => {
      if (handshakeSettled) {
        return;
      }
      handshakeSettled = true;
      clearTimeout(handshakeTimer);
      rejectReady(error);
    };
  });

  return {
    ready,
    pushEvent(event, payload) {
      if (!subscriptions.has(event)) {
        return;
      }
      post({ v: BRIDGE_PROTOCOL_VERSION, type: 'event', event, payload });
    },
    pushAction(cardId, actionId, data) {
      post({ v: BRIDGE_PROTOCOL_VERSION, type: 'action', cardId, actionId, data });
    },
    ping(timeoutMs = 5_000) {
      // destroy 后立即拒绝（post 已静默丢弃，不立即拒绝则调用方只能等超时）
      if (destroyed) {
        return Promise.reject(new Error('Plugin bridge destroyed'));
      }
      const id = `ping-${Date.now().toString(36)}-${++counter}`;
      return new Promise<void>((resolvePing, rejectPing) => {
        const timer = setTimeout(() => {
          pendingPings.delete(id);
          rejectPing(new Error(`Plugin bridge ping timed out after ${timeoutMs}ms`));
        }, timeoutMs);
        pendingPings.set(id, { resolve: resolvePing, reject: rejectPing, timer });
        post({ v: BRIDGE_PROTOCOL_VERSION, type: 'ping', id });
      });
    },
    request(event, payload, timeoutMs = 5_000) {
      if (destroyed) {
        return Promise.reject(new Error('Plugin bridge destroyed'));
      }
      const id = `host-call-${Date.now().toString(36)}-${++counter}`;
      return new Promise<unknown>((resolveReq, rejectReq) => {
        const timer = setTimeout(() => {
          pendingRequests.delete(id);
          rejectReq(new Error(`Plugin bridge host-call "${event}" timed out after ${timeoutMs}ms`));
        }, timeoutMs);
        pendingRequests.set(id, { resolve: resolveReq, reject: rejectReq, timer });
        post({ v: BRIDGE_PROTOCOL_VERSION, type: 'host-call', id, event, payload });
      });
    },
    destroy() {
      if (destroyed) {
        return;
      }
      destroyed = true;
      readyCleanup();
      // 握手未 settle 时以 destroyed 拒绝 ready（幂等：已 settle 则无操作）
      rejectReadyRef(new Error('Plugin bridge destroyed'));
      for (const [id, pending] of pendingPings) {
        clearTimeout(pending.timer);
        pending.reject(new Error('Plugin bridge destroyed'));
        pendingPings.delete(id);
      }
      for (const [id, pending] of pendingRequests) {
        clearTimeout(pending.timer);
        pending.reject(new Error('Plugin bridge destroyed'));
        pendingRequests.delete(id);
      }
      subscriptions.clear();
    }
  };
}
