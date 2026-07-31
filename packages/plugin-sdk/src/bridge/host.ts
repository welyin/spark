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
  handler: BridgeHostHandler;
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
  /** 插件已订阅的事件集合 */
  const subscriptions = new Set<string>();
  const pendingPings = new Map<string, { resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
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
      // 严格校验：origin 与 source 必须同时指向本桥绑定的 iframe
      if (event.origin !== options.expectedOrigin || event.source !== (remote as unknown)) {
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
          Promise.resolve()
            .then(() => options.handler(message.module, message.method, message.args))
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
        case 'event':
          // 插件→宿主上行事件（runtime-error 错误上报等），与握手状态无关
          options.onEvent?.(message.event, message.payload);
          return;
        case 'action':
          // 插件（message-card）→宿主卡片按钮回调上行，归属校验与路由在壳层
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
      subscriptions.clear();
    }
  };
}
