/**
 * 消息页空状态跳转通讯录的意图哨兵。
 * App.vue 的 `spark:open-contact` 事件要求 detail.rootId 非空才会切 tab，
 * 因此用哨兵值表达「仅跳转通讯录 / 跳转并打开添加对话框」两种意图，
 * ContactsPage 消费后按意图处理，不做联系人匹配。
 */
export const CONTACT_INTENT_BROWSE = '__browse__';
export const CONTACT_INTENT_ADD = '__add__';

/** 切到通讯录页并携带意图（App.vue 切 tab，ContactsPage 消费意图） */
export function openContacts(intent: string): void {
  window.dispatchEvent(new CustomEvent('spark:open-contact', { detail: { rootId: intent } }));
}
