/**
 * 通讯录 store（设计 ui-contacts §2/§4/§5/§7/§8）——内核真实数据 + 内存响应式缓存接入层。
 *
 * 数据来源为内核通讯录接口（window.electronAPI.contacts，对齐 api/types.ts 的
 * SpaceContactsDto）：Tauri 环境下首次建空间时异步水合 overview，之后的本地
 * 变更（资料/标签/分组/申请）同步落到内存缓存并 fire-and-forget 持久化回内核；
 * 好友申请事件（FriendRequestReceived/FriendRequestAccepted）经 listenP2pEvents
 * 增量合入缓存。非 Tauri 环境（单测/纯前端开发）完全不触网，退回本地种子数据
 * （seedPersonal/seedOrg，仅非 Tauri 使用）——渲染单测直接依赖种子朋友/分组树。
 *
 * 数据按空间 key 隔离：'personal' 为个人空间，'org:<orgId>' 为组织空间；
 * 组织空间的成员名单本身来自 organization.listMine（真实数据），
 * 这里只存成员的本地附加资料（备注/电话/标签/备忘/照片/拉黑）。
 *
 * 实现按职责拆分到同目录下（本文件仅为门面 re-export，调用方 import 路径不变）：
 * types（共享类型）→ seed（种子数据）→ store（空间单例 + 水合/持久化/事件）
 * → queries（基础查询与资料读写）→ requests/tags/groups（业务函数）。
 */
export type {
  FriendPermission,
  ContactProfile,
  MockFriend,
  ContactTag,
  ContactGroupDef,
  OrgGroupNode,
  RequestMessage,
  FriendRequest,
  SpaceContacts,
  MemberIdentity
} from './types';
export { emptyProfile, spaceKeyOf, memberIdentityOf } from './types';

export { demoContacts, contactsOf } from './store';

export {
  profileOf,
  updateProfile,
  setBlocked,
  addFriend,
  removeFriend,
  resolveRequest,
  requestBadgeCount,
  markRequestRead
} from './queries';

export { sendFriendRequest, retryOutgoing, replyOutgoing, recordOutgoing } from './requests';

export { createTag, renameTag, deleteTag } from './tags';

export {
  setContactGroup,
  createGroup,
  renameGroup,
  deleteGroup,
  moveGroup,
  createOrgGroup,
  renameOrgGroup,
  deleteOrgGroup,
  moveOrgGroupSibling,
  moveOrgGroup
} from './groups';
