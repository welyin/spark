/**
 * 卡片按钮回调路由（plugin_system.md「应用会话」：卡片按钮点击经桥回调插件）。
 *
 * 链路：message-card iframe 内 sdk.messages.triggerCardAction → action 上行
 * （bridge/host onAction）→ AppMessageCard 以**桥绑定的** pluginId/cardId 调
 * routeCardAction（插件自报 cardId 一律忽略）→ 归属校验（cardId 必须是壳层
 * 为该插件该空间签发且当前在架的）→ 同空间主视图实例 host.pushAction →
 * 插件 onCardAction。
 *
 * cardId 编入 spaceKey（`${pluginId}:${spaceKey}:${messageId}`）：归属与路由
 * 均按 pluginId + spaceKey 精确匹配，跨空间同插件卡片互不串投、cardId 不撞。
 *
 * 主视图实例未运行（插件 tab 未打开）时 action 直接丢弃（设计允许，见 TODO.md）。
 */
import type { PluginSpaceContext } from '../../packages/plugin-sdk/src';
import type { BridgeHost } from '../../packages/plugin-sdk/src/bridge/host';

/** 卡片归属（按 pluginId + spaceKey 精确匹配） */
type CardOwner = { pluginId: string; spaceKey: string };

/** 在架卡片：cardId → 归属（AppMessageCard 挂载/卸载时登记） */
const cardOwners = new Map<string, CardOwner>();
/** 主视图实例：`${pluginId}\n${spaceKey}` → 桥宿主（PluginIframeHost 握手完成后登记） */
const mainInstances = new Map<string, BridgeHost>();

/** 空间键（与 mock SpaceKey/桥注入 boundSpaceKey 同口径：'personal' | 'org:<orgId>'） */
export function pluginSpaceKey(space: PluginSpaceContext): string {
  return space.type === 'org' ? `org:${space.id}` : 'personal';
}

function instanceKey(pluginId: string, spaceKey: string): string {
  return `${pluginId}\n${spaceKey}`;
}

/** 登记在架卡片（壳层签发 cardId 时才进入映射——归属校验的数据源） */
export function registerCard(cardId: string, pluginId: string, spaceKey: string): void {
  cardOwners.set(cardId, { pluginId, spaceKey });
}

export function unregisterCard(cardId: string): void {
  cardOwners.delete(cardId);
}

/** 登记插件主视图实例（同插件同空间后登记的覆盖先登记的；卸载时仅清自己） */
export function registerMainViewInstance(pluginId: string, spaceKey: string, host: BridgeHost): void {
  mainInstances.set(instanceKey(pluginId, spaceKey), host);
}

export function unregisterMainViewInstance(pluginId: string, spaceKey: string, host: BridgeHost): void {
  const key = instanceKey(pluginId, spaceKey);
  if (mainInstances.get(key) === host) {
    mainInstances.delete(key);
  }
}

/**
 * 路由卡片回调：归属校验（cardId 属于该插件该空间且在架）后推给同空间主视图实例。
 * 返回是否已送达（false = 归属不符或主实例未运行，调用方仅作观测）。
 *
 * 归属边界本在 AppMessageCard：插件经桥自报的 cardId 在那里即被忽略，传入
 * 本函数的是桥绑定身份派生的壳层签发值；此处 cardOwners 校验为纵深防御
 * （防登记竞态与未来调用方绕过），不是信任边界本身。
 */
export function routeCardAction(
  pluginId: string,
  spaceKey: string,
  cardId: string,
  actionId: string,
  data?: unknown
): boolean {
  const owner = cardOwners.get(cardId);
  if (owner?.pluginId !== pluginId || owner.spaceKey !== spaceKey) {
    return false;
  }
  const host = mainInstances.get(instanceKey(pluginId, spaceKey));
  if (!host) {
    return false;
  }
  host.pushAction(cardId, actionId, data);
  return true;
}
