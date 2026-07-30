/**
 * 基础查询与资料读写：profileOf / updateProfile / 朋友增删 / 收到申请的接受拒绝、
 * 角标与已读。依赖 store（contactsOf/contactsApi），不依赖 requests/tags/groups。
 */
import type { ContactProfile, FriendPermission, MockFriend } from './types';
import { emptyProfile } from './types';
import { contactsApi, contactsOf } from './store';

/** 按 rootId 查朋友条目（无则 undefined）；会话头像等只读场景用 */
export function friendOf(spaceKey: string, rootId: string): MockFriend | undefined {
  return contactsOf(spaceKey).friends.find((item) => item.rootId === rootId);
}

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

/** 删除朋友（设计 §5.5：保留为陌生人，即只删关系不清拉黑状态；block=true 时同时写入拉黑集合） */
export function removeFriend(spaceKey: string, rootId: string, block = false): void {
  const space = contactsOf(spaceKey);
  space.friends = space.friends.filter((item) => item.rootId !== rootId);
  // block=true：本地拉黑标记同步置位（内核侧已独立落拉黑集合；这里让水合前的
  // 本地拉黑列表立即一致，与 setBlocked 同一存储位）
  if (block) {
    profileOf(spaceKey, rootId).blocked = true;
  }
  contactsApi()
    ?.removeFriend(rootId, block)
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

/** 「新的朋友/新的成员」入口角标：列表中未读条目数（收到的 + 我发出的，
 *  每条目最多计 1 次；查看详情后 markRequestRead 清除即不计） */
export function requestBadgeCount(spaceKey: string): number {
  const space = contactsOf(spaceKey);
  return [...space.requests, ...space.outgoing].filter((item) => item.unread).length;
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
