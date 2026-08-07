/**
 * 跨页面「添加朋友 / 添加成员」请求（移动端顶栏「+」菜单用，与 pending-contact 同一模式）。
 *
 * MobileTopBar 的加号菜单点击后：写入本模块并切到通讯录 tab（App.vue 处理），
 * ContactsPage 挂载/监听后消费该请求，打开对应对话框（添加朋友 AddFriendDialog /
 * 添加成员 InviteMemberDialog，复用现有添加流程）。
 */
import { ref } from 'vue';

export type AddContactKind = 'friend' | 'member';

/** 待消费的添加请求；通讯录页消费后置空 */
export const pendingAddContact = ref<AddContactKind | null>(null);

export function requestAddContact(kind: AddContactKind): void {
  pendingAddContact.value = kind;
}

/** 取出并清空当前请求（无请求时返回 null） */
export function consumePendingAddContact(): AddContactKind | null {
  const kind = pendingAddContact.value;
  pendingAddContact.value = null;
  return kind;
}
