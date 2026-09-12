/**
 * 跨页面「打开应用详情」请求（全局搜索跳转用，与 pending-chat 同一模式）。
 *
 * GlobalSearch 等页面请求在应用页打开某应用的详情视图：写入本模块并把 rail
 * 切到应用页（App.vue 监听 `spark:open-app` 事件完成这两步），
 * AppsPage 挂载/监听后消费该请求，在市场条目（真实 + mock 合并列表）中
 * 找到该应用并进入详情视图。
 */
import { ref } from 'vue';

/** 待消费的应用详情请求（插件 id）；应用页消费后置空 */
export const pendingAppDetail = ref<string | null>(null);

export function requestOpenAppDetail(pluginId: string): void {
  pendingAppDetail.value = pluginId;
}

/** 取出并清空当前请求（无请求时返回 null） */
export function consumePendingAppDetail(): string | null {
  const id = pendingAppDetail.value;
  pendingAppDetail.value = null;
  return id;
}

/** 待消费的「为本空间启用」视图请求（空间上下文入口：桌面引导卡 / 手机桌面「添加应用」） */
export const pendingAppsView = ref<'enable' | null>(null);

export function requestOpenAppsView(view: 'enable'): void {
  pendingAppsView.value = view;
}

export function consumePendingAppsView(): 'enable' | null {
  const view = pendingAppsView.value;
  pendingAppsView.value = null;
  return view;
}
