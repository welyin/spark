/**
 * 跨页面「打开联系人资料」请求（全局搜索跳转用，与 pending-chat 同一模式）。
 *
 * GlobalSearch 等页面请求在通讯录右栏打开某联系人的资料面板：写入本模块并
 * 把 rail 切到通讯录页（App.vue 监听 `spark:open-contact` 事件完成这两步），
 * ContactsPage 挂载/监听后消费该请求，在当前空间联系人列表中找到并选中。
 */
import { ref } from 'vue';

export interface PendingContact {
  rootId: string;
}

/** 待消费的打开联系人请求；通讯录页消费后置空 */
export const pendingContact = ref<PendingContact | null>(null);

export function requestOpenContact(target: PendingContact): void {
  pendingContact.value = target;
}

/** 取出并清空当前请求（无请求时返回 null） */
export function consumePendingContact(): PendingContact | null {
  const target = pendingContact.value;
  pendingContact.value = null;
  return target;
}
