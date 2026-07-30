/**
 * 插件桥协议（postMessage 双向 RPC，设计文档「统一 SDK」节）。
 *
 * 插件视图运行在独立 origin 的 iframe 中，SDK 调用经 postMessage 桥异步完成：
 * - 握手：插件侧发 hello（携 sdkVersion/pluginId/viewId），宿主回 ready（携 PluginContext）
 *   或 error（SDK 契约版本不兼容，拒绝加载）；
 * - 调用：插件→宿主 call（module/method/args），宿主→插件 result（ok + data|error）；
 * - 事件：插件→宿主 subscribe/unsubscribe，宿主→插件 event；event 信封亦用于
 *   插件→宿主的上行事件（runtime-error 错误上报，见 bridge/client.ts）；
 * - 熔断配套：宿主→插件 ping，插件侧自动回 pong；
 * - 卡片回调预留：action（消息卡片按钮回调，本波只定义类型，不实现路由）。
 *
 * 全部消息为结构化克隆安全的纯 JSON 对象，信封带协议版本字段 `v: 1`
 * （BRIDGE_PROTOCOL_VERSION；与 SDK 契约版本 sdkVersion 是两个维度：
 * v 描述消息信封格式，sdkVersion 在握手时协商能力集）。
 */

import type { PluginContext } from '../index';

/** 桥协议信封版本（所有消息的 v 字段） */
export const BRIDGE_PROTOCOL_VERSION = 1;

/** 握手失败错误码：SDK 契约版本不兼容（manifest sdkVersion 与宿主不支持） */
export const BRIDGE_ERROR_SDK_VERSION_INCOMPATIBLE = 'sdk-version-incompatible';

/** 调用失败时 result.error 的默认错误码（宿主 handler 抛错） */
export const BRIDGE_ERROR_CALL_FAILED = 'call-failed';

// ------------------------------------------------------------------
// 消息类型
// ------------------------------------------------------------------

/** 调用失败错误载荷（结构化克隆安全） */
export type BridgeCallError = {
  code: string;
  message: string;
};

// ---- 握手 ----

/** 插件→宿主：握手请求 */
export type BridgeHelloMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'hello';
  /** 插件依赖的 SDK 契约版本（manifest.sdkVersion） */
  sdkVersion: string;
  pluginId: string;
  viewId: string;
};

/** 宿主→插件：握手成功，下发运行上下文 */
export type BridgeReadyMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'ready';
  /** 宿主实际提供的 SDK 契约版本 */
  sdkVersion: string;
  ctx: PluginContext;
};

/** 宿主→插件：握手失败（如版本不兼容） */
export type BridgeErrorMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'error';
  code: string;
  message: string;
};

// ---- 调用 ----

/** 插件→宿主：SDK 调用 */
export type BridgeCallMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'call';
  id: string;
  /** SDK 模块名（docs/identity/evidence/p2p/runtime） */
  module: string;
  method: string;
  args: unknown[];
};

/** 宿主→插件：调用/订阅结果（subscribe/unsubscribe 也以 result 应答） */
export type BridgeResultMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'result';
  id: string;
  ok: boolean;
  data?: unknown;
  error?: BridgeCallError;
};

// ---- 事件 ----

/** 插件→宿主：订阅系统事件 */
export type BridgeSubscribeMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'subscribe';
  id: string;
  event: string;
};

/** 插件→宿主：取消订阅系统事件 */
export type BridgeUnsubscribeMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'unsubscribe';
  id: string;
  event: string;
};

/** 宿主→插件：事件推送 */
export type BridgeEventMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'event';
  event: string;
  payload?: unknown;
};

// ---- 心跳（熔断配套） ----

/** 宿主→插件：心跳探测 */
export type BridgePingMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'ping';
  id: string;
};

/** 插件→宿主：心跳应答（插件侧自动回复） */
export type BridgePongMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'pong';
  id: string;
};

// ---- 卡片回调（预留，本波只定义类型不实现路由） ----

/** 宿主→插件：消息卡片按钮回调 */
export type BridgeActionMessage = {
  v: typeof BRIDGE_PROTOCOL_VERSION;
  type: 'action';
  cardId: string;
  actionId: string;
  data?: unknown;
};

export type PluginToHostMessage =
  | BridgeHelloMessage
  | BridgeCallMessage
  | BridgeSubscribeMessage
  | BridgeUnsubscribeMessage
  | BridgePongMessage;

export type HostToPluginMessage =
  | BridgeReadyMessage
  | BridgeErrorMessage
  | BridgeResultMessage
  | BridgeEventMessage
  | BridgePingMessage
  | BridgeActionMessage;

export type BridgeMessage = PluginToHostMessage | HostToPluginMessage;

// ------------------------------------------------------------------
// 编解码（postMessage 直接承载对象，结构化克隆即「编码」；此处为接收侧校验解码）
// ------------------------------------------------------------------

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0;
}

function isCallError(value: unknown): value is BridgeCallError {
  return isRecord(value) && typeof value.code === 'string' && typeof value.message === 'string';
}

/**
 * 解析并校验一条桥消息：结构不合法（非对象、版本不符、类型未知、必填字段缺失）
 * 一律返回 null，接收侧直接丢弃——不抛异常，避免伪造消息打断监听循环。
 */
export function parseBridgeMessage(data: unknown): BridgeMessage | null {
  if (!isRecord(data)) {
    return null;
  }
  if (data.v !== BRIDGE_PROTOCOL_VERSION) {
    return null;
  }

  switch (data.type) {
    case 'hello':
      return isString(data.sdkVersion) && isString(data.pluginId) && isString(data.viewId)
        ? (data as unknown as BridgeHelloMessage)
        : null;
    case 'ready':
      return isString(data.sdkVersion) && isRecord(data.ctx)
        ? (data as unknown as BridgeReadyMessage)
        : null;
    case 'error':
      return typeof data.code === 'string' && typeof data.message === 'string'
        ? (data as unknown as BridgeErrorMessage)
        : null;
    case 'call':
      return isString(data.id) && isString(data.module) && isString(data.method) && Array.isArray(data.args)
        ? (data as unknown as BridgeCallMessage)
        : null;
    case 'result':
      if (!isString(data.id) || typeof data.ok !== 'boolean') {
        return null;
      }
      if (data.error !== undefined && !isCallError(data.error)) {
        return null;
      }
      return data as unknown as BridgeResultMessage;
    case 'subscribe':
    case 'unsubscribe':
      return isString(data.id) && isString(data.event)
        ? (data as unknown as BridgeSubscribeMessage | BridgeUnsubscribeMessage)
        : null;
    case 'event':
      return isString(data.event) ? (data as unknown as BridgeEventMessage) : null;
    case 'ping':
    case 'pong':
      return isString(data.id)
        ? (data as unknown as BridgePingMessage | BridgePongMessage)
        : null;
    case 'action':
      return isString(data.cardId) && isString(data.actionId)
        ? (data as unknown as BridgeActionMessage)
        : null;
    default:
      return null;
  }
}

/**
 * SDK 契约版本协商：v1 协议下要求精确一致（宿主与 manifest sdkVersion 均为 '1'）。
 * 未来版本演进时在此扩展兼容矩阵。
 */
export function isSdkVersionCompatible(hostSdkVersion: string, pluginSdkVersion: string): boolean {
  return hostSdkVersion === pluginSdkVersion;
}
