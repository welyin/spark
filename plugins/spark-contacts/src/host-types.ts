/**
 * 宿主接口本地类型（spark-contacts 自包含）：与壳层 api/types.ts 的通讯录/
 * 组织域 DTO 逐字段同形（结构类型天然兼容）。
 *
 * 边界纪律（plugins/README.md）：插件禁止 import 壳层 app/src 任何模块——
 * 桥/宿主面 DTO 在插件内保留本地同形拷贝，壳层侧变更时按等语义纪律同步。
 */
import type {
  PluginContactGroup,
  PluginContactOverview,
  PluginContactTag,
  PluginFriend,
  PluginFriendRequest,
  PluginOrgGroupNode
} from '../../../packages/plugin-sdk/src';

/** 朋友 DTO（与壳层 FriendDto 同形；与 SDK PluginFriend 同源） */
export type FriendDto = PluginFriend;

/** 朋友/成员申请 DTO（与壳层 FriendRequestDto 同形；与 SDK PluginFriendRequest 同源） */
export type FriendRequestDto = PluginFriendRequest;

/** 组织邀请记录（与壳层 OrgInviteRecordDto 同形；出/入站共用） */
export type OrgInviteRecordDto = {
  id: string;
  orgId: string;
  orgName: string;
  /** 组织 logo（data URL）；可省 */
  orgAvatar?: string;
  /** 对端 rootId（outgoing=被邀请人；incoming=邀请人） */
  peerRootId: string;
  peerNickname: string;
  direction: 'outgoing' | 'incoming';
  status: 'pending' | 'accepted' | 'declined';
  createdAt: number;
  updatedAt: number;
};

/** 单空间通讯录总览 DTO（与壳层 SpaceContactsDto 同形；SDK overview 运行期同形） */
export interface SpaceContactsDto {
  friends: FriendDto[];
  requests: FriendRequestDto[];
  outgoing: FriendRequestDto[];
  tags: PluginContactTag[];
  groups: PluginContactGroup[];
  groupTree: PluginOrgGroupNode[];
  memberExtras: PluginContactOverview['memberExtras'];
}

/** 通讯录域 P2P 事件最小同形（store 按 kind 判别联合消费；data 形状随 kind 定） */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type P2pEventDto = { kind: string; data: any };

/** 组织节点寻址（端点化：单设备对象 / 多设备数组） */
export type OrgNodeInfo =
  | { deviceUid?: string; peerId?: string; addresses: string[] }
  | Array<{ deviceUid?: string; peerId?: string; addresses: string[] }>;

/** 组织视图（与壳层 OrgView 的插件消费面子集同形；SDK runtime.listMineOrganizations 元素） */
export type OrgView = {
  orgId: string;
  name: string;
  description: string;
  /** 组织 logo（data URL）；可能缺省/空串 */
  avatar?: string;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    joinedAt: number;
    addedBy: string;
    nodeInfo?: OrgNodeInfo;
  }>;
  currentUserRole: 'admin' | 'member' | null;
  isCurrentUserAdmin: boolean;
  memberCount: number;
  adminCount: number;
};

/**
 * 宿主通讯录接口（与壳层 ElectronAPI['contacts'] 同签名）：store 的数据源形状。
 * 插件内由 sdk-host 以 sdk.contacts 适配实现（space 已由桥绑定，spaceKey 实参透传忽略）。
 */
export interface HostContactsApi {
  overview: (spaceKey: string) => Promise<SpaceContactsDto>;
  updateProfile: (spaceKey: string, rootId: string, patch: Record<string, unknown>) => Promise<{ success: boolean }>;
  setBlocked: (spaceKey: string, rootId: string, blocked: boolean) => Promise<{ success: boolean }>;
  removeFriend: (rootId: string, block?: boolean) => Promise<{ success: boolean }>;
  sendRequest: (input: { id: string; rootId: string; raw: string; peerId?: string; addresses?: string[]; source: string; message: string }) => Promise<FriendRequestDto>;
  replyRequest: (requestId: string, text: string) => Promise<FriendRequestDto>;
  askRequest: (requestId: string, text: string) => Promise<FriendRequestDto>;
  resolveRequest: (requestId: string, accept: boolean, permission: 'open' | 'chatOnly') => Promise<{ success: boolean }>;
  tagCreate: (spaceKey: string, id: string, name: string) => Promise<PluginContactTag>;
  tagRename: (spaceKey: string, tagId: string, name: string) => Promise<{ success: boolean }>;
  tagDelete: (spaceKey: string, tagId: string) => Promise<{ success: boolean }>;
  groupCreate: (spaceKey: string, id: string, name: string) => Promise<PluginContactGroup>;
  groupRename: (spaceKey: string, groupId: string, name: string) => Promise<{ success: boolean }>;
  groupDelete: (spaceKey: string, groupId: string) => Promise<{ success: boolean }>;
  groupMove: (spaceKey: string, groupId: string, toIndex: number) => Promise<{ success: boolean }>;
  setGroup: (spaceKey: string, rootId: string, groupId: string) => Promise<{ success: boolean }>;
  orgGroupCreate: (spaceKey: string, parentId: string, id: string, name: string) => Promise<PluginOrgGroupNode | null>;
  orgGroupRename: (spaceKey: string, id: string, name: string) => Promise<{ success: boolean }>;
  orgGroupDelete: (spaceKey: string, id: string) => Promise<{ success: boolean }>;
  orgGroupMove: (spaceKey: string, id: string, toIndex: number, newParentId?: string) => Promise<{ success: boolean }>;
}

/**
 * 宿主组织接口（与壳层 ElectronAPI['organization'] 的插件消费面子集同签名）。
 * v1 边界：邀请记录（inviteRecords）未进 SDK 面（organization 域接口面变更
 * 待拍板，A19 遗留）——sdk-host 返回 undefined，组织空间「我发出的邀请」
 * 面板退化为仅 overview 数据。
 */
export interface HostOrganizationApi {
  inviteRecords: (orgId: string) => Promise<OrgInviteRecordDto[]>;
}
