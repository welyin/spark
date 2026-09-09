/**
 * 全局深链服务（阶段 3 / ui-architecture §4.4 deep-link 应用内统一入口）。
 *
 * 全系统只有一套「跳到本体」的寻址逻辑（README §6.6）：消息卡片 / 事务列表 / 通知中心 /
 * 系统快捷入口共用。应用内段 = 打开某插件主视图并携带视图引导（viewBootstrap.cardData），
 * 经全局事件 `spark:open-plugin` 派发给 App.vue（openPluginTab 渲染 PluginIframeHost）。
 *
 * 外部唤起（spark:// URL / Universal Link / 系统快捷方式）属 2.11 原生侧，落到同一事件。
 *
 * 定稿寻址语法（shell-desktop §3.7）：spark://space/<域>/app/<插件>[/object/<对象>]；
 * 应用内以结构化 payload 表达（domain/objectId 等），URL 解析层（原生侧）映射到同一 payload。
 */

/** 打开插件视图请求（应用内深链 payload） */
export interface OpenPluginDeepLink {
  /** 插件 id（仓库规范化地址 / manifest id） */
  pluginId: string;
  /** 视图引导（注入插件 window.__sparkPluginView.cardData，如 { affairId } / { postId }） */
  cardData?: unknown;
  /** 目标视图 id（缺省插件 entryView） */
  viewId?: string;
  /** 来源动作 id（观测/日志用，可选） */
  actionId?: string;
}

export const OPEN_PLUGIN_DEEPLINK_EVENT = 'spark:open-plugin';

/** 经统一深链打开插件主视图（消息卡片回退 / 事务分发 / 通知中心共用） */
export function openPluginDeepLink(link: OpenPluginDeepLink): void {
  window.dispatchEvent(new CustomEvent(OPEN_PLUGIN_DEEPLINK_EVENT, { detail: link }));
}
