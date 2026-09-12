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
 *
 * D9 携域深链：payload 可带目标空间（space），openPluginDeepLink 在派发事件前先切到该域——
 * 消费方（App.vue onOpenPluginDeepLink → openPluginTab）与上下文条（TopNavbar 随
 * currentSpace 响应）读取的都是切域后的状态，实现「先切域再开目标、上下文条同步切域」。
 * 调用方不带 space 时维持现状（在当前域打开）。
 */
import { currentSpace, switchToOrg, switchToPersonal } from '../stores/current-space';

/** 深链目标空间（与 stores/current-space 的 CurrentSpace 同形，此处独立声明避免服务层依赖 store 类型） */
export type DeepLinkSpace = { type: 'personal' } | { type: 'org'; orgId: string };

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
  /** D9 目标空间：携带时先切到该域桌面再打开（消息/事务卡片跳出域的统一切域点） */
  space?: DeepLinkSpace;
}

export const OPEN_PLUGIN_DEEPLINK_EVENT = 'spark:open-plugin';

/** 经统一深链打开插件主视图（消息卡片回退 / 事务分发 / 通知中心共用）；
    携带 space 时先切到目标域（上下文条随 currentSpace 同步切域），再派发打开事件 */
export function openPluginDeepLink(link: OpenPluginDeepLink): void {
  if (link.space) {
    const current = currentSpace.value;
    const same =
      link.space.type === 'personal'
        ? current.type === 'personal'
        : current.type === 'org' && current.orgId === link.space.orgId;
    if (!same) {
      if (link.space.type === 'personal') {
        switchToPersonal();
      } else {
        switchToOrg(link.space.orgId);
      }
    }
  }
  window.dispatchEvent(new CustomEvent(OPEN_PLUGIN_DEEPLINK_EVENT, { detail: link }));
}

// ------------------------------------------------------------------
// X8 复制 spark:// 深链（空间 / 应用 / 对象三粒度）
// ------------------------------------------------------------------

/** spark:// URL 段编码：域 = 个人空间 'personal' / 组织 orgId（URL 安全化） */
function encodeSegment(value: string): string {
  return encodeURIComponent(value);
}

/**
 * 生成 spark:// 深链（语法见文件头）：
 * - 空间粒度：spark://space/<域>
 * - 应用粒度：spark://space/<域>/app/<插件>
 * - 对象粒度：spark://space/<域>/app/<插件>/object/<对象>
 * 安全口径（X9）：URL 只含寻址字段（orgId / 插件 id / 对象 id），不含密钥或内容数据；
 * 外部唤起仍需解锁与成员资格校验（原生侧协议注册待 2.11 排期）。
 */
export function buildSparkUrl(space: DeepLinkSpace, pluginId?: string, objectId?: string): string {
  const domain = space.type === 'personal' ? 'personal' : encodeSegment(space.orgId);
  let url = `spark://space/${domain}`;
  if (pluginId) {
    url += `/app/${encodeSegment(pluginId)}`;
    if (objectId) {
      url += `/object/${encodeSegment(objectId)}`;
    }
  }
  return url;
}
