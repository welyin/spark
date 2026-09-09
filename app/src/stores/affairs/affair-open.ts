/**
 * 事务打开分发（阶段 3 / ui-architecture §4.4 deep-link 应用内段，README §4.4「列表归外壳、单个事务归插件」）。
 *
 * 点事务卡片 → 按事务类型查类型注册表（affair-types，宿主自建）→ 找到承接插件：
 * 先切域（若事务锚定了域）、再打开该插件主视图并传入 affairId（pending 模式，同 pending-chat）。
 * 本机没装能处理该类型的插件 → 空状态引导去应用市场（如同系统没有打开某类文件的程序）。
 *
 * 类型判定：当前 affairs 协议未在创世记录强制类型标识（spark-affairs 为参考实现），
 * 第一版以「已装且声明承接任一类型」的插件为兜底承接方；待协议带类型标识后按 affairType 精确路由。
 */
import { ref } from 'vue';
import { currentSpace, switchToOrg, switchToPersonal } from '../current-space';
import { rebuildAffairTypes, affairTypeRegistry } from './affair-types';
import { openPluginDeepLink } from '../../services/deep-link';
import type { AffairFeedItem } from './affair-feed';

/** 待打开的事务（pending 模式：插件主视图挂载后消费，同 pending-chat/pending-contact） */
export const pendingAffair = ref<{ affairId: string; pluginId: string } | null>(null);

/** 打开事务请求结果（供 UI 给「未装插件」空状态引导） */
export type OpenAffairResult =
  | { ok: true; pluginId: string }
  | { ok: false; reason: 'no-plugin' };

/**
 * 打开事务：找承接插件 → （如需）切域 → 经统一深链（deep-link）打开该插件主视图并携带 affairId。
 * 当前协议无类型标识，取注册表第一个已声明承接的插件（参考实现 spark-affairs）。
 */
export async function openAffairInPlugin(item: AffairFeedItem): Promise<OpenAffairResult> {
  // 确保注册表已建（首次进入事务页可能未扫）
  if (affairTypeRegistry.value.size === 0) {
    await rebuildAffairTypes();
  }
  // 兜底承接：取注册表第一个插件（协议带类型后改为 pluginForAffairType(item.type)）
  const pluginId = affairTypeRegistry.value.size > 0 ? [...affairTypeRegistry.value.values()][0] : null;
  if (!pluginId) {
    return { ok: false, reason: 'no-plugin' };
  }

  // 切域（当前 spark-affairs 为 personal 空间插件；锚定域语义就绪后按 item 所属域切换）
  if (currentSpace.value.type !== 'personal') {
    switchToPersonal();
  }

  pendingAffair.value = { affairId: item.affairId, pluginId };
  openPluginDeepLink({ pluginId, cardData: { affairId: item.affairId } });
  return { ok: true, pluginId };
}

/** 插件主视图消费 pending 事务（读取后清空，同 consumePendingChat） */
export function consumePendingAffair(): { affairId: string; pluginId: string } | null {
  const value = pendingAffair.value;
  pendingAffair.value = null;
  return value;
}

// 供切域后恢复（org 空间语义预留；当前 personal 兜底）
export { switchToOrg, switchToPersonal };
