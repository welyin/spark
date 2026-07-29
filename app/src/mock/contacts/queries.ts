/**
 * 基础查询与资料读写：profileOf / updateProfile / 朋友增删 / 收到申请的接受拒绝、
 * 角标与已读。依赖 store（contactsOf/contactsApi），不依赖 requests/tags/groups。
 */
import type { ContactProfile, FriendPermission, MockFriend } from './types';
import { emptyProfile } from './types';
import { contactsApi, contactsOf } from './store';

/** 取联系人的本地资料：个人空间读朋友条目，组织空间读成员附加资料（惰性建默认） */
export function profileOf(spaceKey: string, rootId: string): ContactProfile {
  const space = contactsOf(spaceKey);
  const friend = space.friends.find((item) => item.rootId === rootId);
  if (friend) {
    return friend;
  }
  if (!space.memberExtras[rootId]) {
    space.memberExtras[rootId] = emptyProfile();
  }
  return space.memberExtras[rootId];
}

/** 更新本地资料（设置备注和标签对话框保存入口） */
export function updateProfile(spaceKey: string, rootId: string, patch: Partial<ContactProfile>): void {
  Object.assign(profileOf(spaceKey, rootId), patch);
  contactsApi()
    ?.updateProfile(spaceKey, rootId, patch)
    .catch(() => {});
}

export function setBlocked(spaceKey: string, rootId: string, blocked: boolean): void {
  profileOf(spaceKey, rootId).blocked = blocked;
  contactsApi()
    ?.setBlocked(spaceKey, rootId, blocked)
    .catch(() => {});
}

/** 添加朋友本地入口：接受申请时本地立即落（后端 resolveRequest 已建关系，无需单独 api） */
export function addFriend(spaceKey: string, rootId: string, nickname: string): MockFriend {
  const space = contactsOf(spaceKey);
  const existing = space.friends.find((item) => item.rootId === rootId);
  if (existing) {
    return existing;
  }
  const friend: MockFriend = {
    ...emptyProfile(),
    rootId,
    nickname: nickname || `${rootId.slice(0, 8)}...`,
    signature: '',
    addedAt: Date.now()
  };
  space.friends.push(friend);
  return friend;
}

/** 删除朋友（设计 §5.5：保留为陌生人，即只删关系不清拉黑状态） */
export function removeFriend(spaceKey: string, rootId: string): void {
  const space = contactsOf(spaceKey);
  space.friends = space.friends.filter((item) => item.rootId !== rootId);
  contactsApi()
    ?.removeFriend(rootId)
    .catch(() => {});
}

// 接受时带上「向其开放的权限」（§6，仅个人空间生效），写入新朋友的本地资料
export function resolveRequest(spaceKey: string, requestId: string, accept: boolean, permission: FriendPermission = 'open'): void {
  const space = contactsOf(spaceKey);
  const request = space.requests.find((item) => item.id === requestId);
  if (!request || request.status !== 'pending') {
    return;
  }
  request.status = accept ? 'accepted' : 'ignored';
  request.updatedAt = Date.now();
  if (accept && spaceKey === 'personal') {
    addFriend(spaceKey, request.rootId, request.nickname);
    profileOf(spaceKey, request.rootId).permission = permission;
  }
  contactsApi()
    ?.resolveRequest(requestId, accept, permission)
    .catch(() => {});
}

/** 「新的朋友/新的成员」入口角标：待处理的收到申请 + 任何未读新变化 */
export function requestBadgeCount(spaceKey: string): number {
  const space = contactsOf(spaceKey);
  const pending = space.requests.filter((item) => item.status === 'pending').length;
  const unread = [...space.requests, ...space.outgoing].filter((item) => item.unread).length;
  return pending + unread;
}

/** 查看申请详情后清除未读（收到的与我发出的共用 id 前缀，一并扫） */
export function markRequestRead(spaceKey: string, requestId: string): void {
  const space = contactsOf(spaceKey);
  for (const item of [...space.requests, ...space.outgoing]) {
    if (item.id === requestId) {
      item.unread = false;
    }
  }
}
