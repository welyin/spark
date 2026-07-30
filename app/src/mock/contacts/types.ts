/**
 * 通讯录共享类型与基础工具：全部空间的缓存模型、空资料工厂、空间 key 约定。
 * 本文件不依赖任何其他 contacts 模块，
 * 处于依赖方向最底层（types <- seed/store <- queries <- requests/tags/groups）。
 */

/** 朋友权限（设计 §6：开放 / 仅聊天），仅个人空间使用 */
export type FriendPermission = 'open' | 'chatOnly';

/** 联系人本地资料（备注名优先展示；电话/标签/备忘/照片均仅自己可见，设计 §5.4） */
export type ContactProfile = {
  remark: string;
  phones: string[];
  tagIds: string[];
  /** 所属分组：个人空间=ContactGroupDef.id，组织空间=树节点 id；'' = 未分组（出现在「未分组」虚拟组） */
  groupId: string;
  memo: string;
  /** TODO(mock): 照片只存占位标记（'photo-N'），未接入真实上传 */
  photos: string[];
  permission: FriendPermission;
  blocked: boolean;
};

/** 个人空间朋友 */
export type MockFriend = ContactProfile & {
  rootId: string;
  nickname: string;
  signature: string;
  /** 对方性别（详情页昵称旁图标；缺省则不显示） */
  gender?: 'male' | 'female';
  /** 对端同步过来的头像（data URL）；缺省走自动头像 */
  avatar?: string;
  addedAt: number;
};

/** 通讯录标签（设计 §8） */
export type ContactTag = { id: string; name: string };

/** 个人空间分组（扁平一层，数组顺序即显示顺序；「未分组」为虚拟组不入列） */
export type ContactGroupDef = { id: string; name: string };

/** 组织空间分组树节点（数组顺序即同级排序；仅管理员可改树结构，任何人可同级拖拽排序） */
export type OrgGroupNode = { id: string; name: string; children: OrgGroupNode[] };

/** 申请上的来回回复消息（对方问「你是谁」、我回答、对方再问……直到接受/拒绝） */
export type RequestMessage = { from: 'me' | 'peer'; text: string; ts: number };

/** 新的朋友/新的成员申请（设计 §2.2 功能区第一行） */
export type FriendRequest = {
  id: string;
  rootId: string;
  nickname: string;
  message: string;
  source: string;
  /** 状态（declined 为组织邀请对方拒绝的终态，个人空间申请不产生） */
  status: 'pending' | 'accepted' | 'ignored' | 'replied' | 'failed' | 'declined';
  /** 申请发出/收到时间 */
  createdAt: number;
  /** 最近一次状态变化/新回复时间（列表按此倒序，有任何新变化都冒泡到顶部） */
  updatedAt: number;
  /** 有未看的新变化（新申请、对方回复、接受/拒绝/连接失败），查看后清除 */
  unread?: boolean;
  /** 来回回复记录（对方回复询问后双方可继续互复，直到对方拒绝/接受） */
  thread?: RequestMessage[];
  /** 组织邀请码（我发出的组织成员邀请，待对方凭码加入时可再复制） */
  inviteCode?: string;
  /** 对端同步过来的头像（data URL）；缺省走自动头像 */
  avatar?: string;
};

export type SpaceContacts = {
  friends: MockFriend[];
  /** 收到的申请（「新的朋友/新的成员」列表） */
  requests: FriendRequest[];
  /** 我发出的申请（添加朋友/添加为个人联系人，等待对方确认） */
  outgoing: FriendRequest[];
  tags: ContactTag[];
  /** 个人空间：扁平分组（组织空间不用） */
  groups: ContactGroupDef[];
  /** 组织空间：树形分组（个人空间不用） */
  groupTree: OrgGroupNode[];
  /** 组织空间：成员 rootId -> 本地附加资料 */
  memberExtras: Record<string, ContactProfile>;
};

export function emptyProfile(): ContactProfile {
  return { remark: '', phones: [], tagIds: [], groupId: '', memo: '', photos: [], permission: 'open', blocked: false };
}

/** 个人空间 key 固定为 'personal'，组织空间为 'org:<orgId>'（唯一定义在 mock/space-key.ts，此处 re-export） */
export { spaceKeyOf } from '../space-key';

// ------------------------------------------------------------------
// 组织成员的组织身份（昵称/性别/签名）。
// TODO(mock): 内核组织成员已携带真实身份字段（OrganizationMember 的
// nickname/avatar/signature/gender/region，经快照传播，F2 落地）；本池仍按
// rootId 哈希从固定池里确定性取假数据（同一成员恒定），是 mock 的有意降级
// ——消费端 use-contacts-data 属冻结前端，整体替换待前端排期（ui-space-navbar §9.2）
// ------------------------------------------------------------------

export type MemberIdentity = {
  nickname: string;
  signature: string;
  gender?: 'male' | 'female';
};

const MEMBER_NAME_POOL = [
  '张伟', '李娜', '王芳', '刘洋', '陈静', '杨帆',
  '赵磊', '黄敏', '周涛', '吴倩', '徐斌', '孙悦'
];

const MEMBER_SIGNATURE_POOL = [
  '越努力越幸运', '静水流深', '今天也要加油', '保持热爱',
  '行胜于言', '', '慢慢来吧', '专注当下'
];

const hashText = (text: string): number => {
  let hash = 0;
  for (let i = 0; i < text.length; i++) {
    hash = (hash * 31 + text.charCodeAt(i)) >>> 0;
  }
  return hash;
};

/** 组织成员的组织身份（确定性 mock：同一 spaceKey+rootId 恒定） */
export function memberIdentityOf(spaceKey: string, rootId: string): MemberIdentity {
  const hash = hashText(`${spaceKey}:${rootId}`);
  return {
    nickname: MEMBER_NAME_POOL[hash % MEMBER_NAME_POOL.length],
    signature: MEMBER_SIGNATURE_POOL[hash % MEMBER_SIGNATURE_POOL.length],
    gender: hash % 3 === 0 ? undefined : hash % 2 === 0 ? 'male' : 'female'
  };
}
