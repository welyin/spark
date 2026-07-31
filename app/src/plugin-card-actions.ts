/**
 * 卡片按钮回调路由（plugin_system.md「应用会话」：卡片按钮点击经桥回调插件）。
 *
 * 链路：message-card iframe 内 sdk.messages.triggerCardAction → action 上行
 * （bridge/host onAction）→ AppMessageCard 以**桥绑定的** pluginId/cardId 调
 * routeCardAction（插件自报 cardId 一律忽略）→ 归属校验（cardId 必须是壳层
 * 为该插件签发且当前在架的）→ 主视图实例 host.pushAction → 插件 onCardAction。
 *
 * 主视图实例未运行（插件 tab 未打开）时 action 直接丢弃（设计允许，见 TODO.md）。
 */
import type { BridgeHost } from '../../packages/plugin-sdk/src/bridge/host';

/** 在架卡片：cardId → 所属 pluginId（AppMessageCard 挂载/卸载时登记） */
const cardOwners = new Map<string, string>();
/** 主视图实例：`${pluginId}\n${spaceId}` → 桥宿主（PluginIframeHost 握手完成后登记） */
const mainInstances = new Map<string, BridgeHost>();

function instanceKey(pluginId: string, spaceId: string): string {
  return `${pluginId}\n${spaceId}`;
}

/** 登记在架卡片（壳层签发 cardId 时才进入映射——归属校验的数据源） */
export function registerCard(cardId: string, pluginId: string): void {
  cardOwners.set(cardId, pluginId);
}

export function unregisterCard(cardId: string): void {
  cardOwners.delete(cardId);
}

/** 登记插件主视图实例（同插件同空间后登记的覆盖先登记的；卸载时仅清自己） */
export function registerMainViewInstance(pluginId: string, spaceId: string, host: BridgeHost): void {
  mainInstances.set(instanceKey(pluginId, spaceId), host);
}

export function unregisterMainViewInstance(pluginId: string, spaceId: string, host: BridgeHost): void {
  const key = instanceKey(pluginId, spaceId);
  if (mainInstances.get(key) === host) {
    mainInstances.delete(key);
  }
}

/**
 * 路由卡片回调：归属校验（cardId 属于该插件且在架）后推给主视图实例。
 * 返回是否已送达（false = 归属不符或主实例未运行，调用方仅作观测）。
 */
export function routeCardAction(pluginId: string, cardId: string, actionId: string, data?: unknown): boolean {
  if (cardOwners.get(cardId) !== pluginId) {
    return false;
  }
  // 卡片不携带空间信息：按 pluginId 取当前在架的主视图实例
  const host = [...mainInstances.entries()].find(([key]) => key.startsWith(`${pluginId}\n`))?.[1];
  if (!host) {
    return false;
  }
  host.pushAction(cardId, actionId, data);
  return true;
}
