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

/** 打开/创建 1:1 会话（App.vue 消费 `spark:open-chat`：记录请求并切到消息页，§5.3）。
 *  所有「去找他聊天」的入口（通讯录资料卡/新朋友面板/全局搜索）统一从这里派发，
 *  不再各自裸写 CustomEvent。name 为首建会话的兜底标题，conversationId 用于定位已存在的会话。 */
export function openChat(detail: { rootId: string; name?: string; conversationId?: string }): void {
  window.dispatchEvent(new CustomEvent('spark:open-chat', { detail }));
}
