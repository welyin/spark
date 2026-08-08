/**
 * 插件 bot 在线状态注册表。
 *
 * bot 联系人没有 P2P peer，会话 online 字段对 bot 恒 false（内核无法判定）。
 * 真实的"在线"语义 = 该插件的后台常驻视图已握手成功、消息监听正在运行。
 * 本模块维护"哪些插件的 bot 当前在线"，由 PluginBackgroundHost 在握手
 * 成功/销毁时更新，由会话/联系人展示层查询。
 */
import { reactive } from 'vue';
import type { BridgeHost } from '../../../packages/plugin-sdk/src/bridge/host';

/** 在线插件集合（pluginId）。响应式，供会话列表/聊天头部 computed 订阅 */
const onlinePlugins = reactive<Set<string>>(new Set());

/** 后台视图的桥实例（pluginId → BridgeHost），供宿主向插件发反向调用（host.request） */
const backgroundHosts = new Map<string, BridgeHost>();

/** 登记后台桥实例（握手成功时调用） */
export function registerBackgroundHost(pluginId: string, host: BridgeHost): void {
  backgroundHosts.set(pluginId, host);
}

/** 注销后台桥实例（宿主销毁时调用） */
export function unregisterBackgroundHost(pluginId: string): void {
  backgroundHosts.delete(pluginId);
}

/**
 * 向插件后台视图发反向调用（host.request）。插件不在线时返回 null。
 * 供删除 bot 联系人前向插件求证「该 bot 是否还存在」。
 */
export async function queryBackgroundPlugin(
  pluginId: string,
  event: string,
  payload?: unknown,
  timeoutMs = 3_000
): Promise<unknown | null> {
  const host = backgroundHosts.get(pluginId);
  if (!host) return null;
  try {
    return await host.request(event, payload, timeoutMs);
  } catch {
    return null; // 无处理器/超时/插件异常一律视为「无答复」
  }
}

/** 标记插件后台视图上线（握手成功时调用） */
export function markBotOnline(pluginId: string): void {
  onlinePlugins.add(pluginId);
}

/** 标记插件后台视图下线（宿主销毁/握手失败时调用） */
export function markBotOffline(pluginId: string): void {
  onlinePlugins.delete(pluginId);
}

/** 某插件的 bot 是否在线 */
export function isBotOnline(pluginId: string): boolean {
  return onlinePlugins.has(pluginId);
}

/**
 * 按 bot 联系人 rootId（`bot:{pluginId}:{botId}`）判定在线。
 * 供会话/联系人展示层直接以 contactId/peerId 查询。
 */
export function isBotContactOnline(botRootId: string): boolean {
  if (!botRootId.startsWith('bot:')) return false;
  const pluginId = botRootId.split(':')[1];
  return pluginId ? onlinePlugins.has(pluginId) : false;
}
