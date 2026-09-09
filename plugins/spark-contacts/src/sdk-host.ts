/**
 * spark-contacts 数据面适配层：把壳层 `mock/contacts` 依赖的宿主接口
 * （`contactsApi()` / `organizationApi()` / `listenP2pEvents()` / `isTauri()`）
 * 以 **同签名** 桥接到插件 SDK（A18/A19 sdk.contacts + sdk.runtime），store
 * 主体零改动（功能对等迁移的关键：同一组件树，只换数据源）。
 *
 * space 由桥绑定（插件实例按空间创建，壳层切换空间时重建实例并注入新
 * PluginContext）——适配层接收的 spaceKey 形参与绑定值一致时透传，
 * 不一致按绑定值执行（与桥「插件自报一律忽略」同口径）。
 *
 * v1 边界（A19 遗留，等 organization 域 SDK 面拍板后补齐）：
 * - organizationApi（组织邀请记录 inviteRecords / 邀请发送）：未进 SDK 面，
 *   返回 undefined——组织空间「新的成员 → 我发出的邀请」面板退化为仅
 *   overview 数据，「添加成员」入口在 ContactsApp 组织分支不渲染。
 */
import type { PluginContext, PluginSDK } from '../../../packages/plugin-sdk/src';
import type {
  HostContactsApi,
  HostOrganizationApi,
  OrgView,
  P2pEventDto,
  SpaceContactsDto
} from './host-types';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定运行上下文（index.ts 握手成功后调用） */
export function bindPluginRuntime(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 当前绑定空间（桥 ready 下发的权威值） */
export function pluginSpace(): PluginContext['space'] {
  return _ctx?.space ?? { type: 'personal', id: 'personal' };
}

/** 空间 key（`personal` / `org:{orgId}`），与壳层 spaceKeyOf 同口径 */
export function spaceKeyOf(space: PluginContext['space'] = pluginSpace()): string {
  return space.type === 'org' ? `org:${space.id}` : 'personal';
}

/** 当前绑定 spaceKey（store/组件的主键入参） */
export function boundSpaceKey(): string {
  return spaceKeyOf();
}

/** 宿主可用性（适配 store 的 isTauri 守卫）：SDK 已绑定即为真；
 *  未绑定（vitest / 纯前端预览）为假——store 退化为本地种子数据 */
export function isTauri(): boolean {
  return _sdk !== null;
}

/** 桥上下文 SDK 句柄（org-membership/current-user 等宿主门面消费） */
export function hostSdk(): PluginSDK | null {
  return _sdk;
}

/**
 * 内核通讯录接口的 SDK 适配（HostContactsApi 同签名，与壳层
 * ElectronAPI['contacts'] 等语义）：每个方法忽略 spaceKey 实参（space 已由
 * 桥绑定）；等语义纪律下实现即 A18/A19 SDK 调用直通，无任何行为加工。
 */
export function contactsApi(): HostContactsApi | undefined {
  const sdk = _sdk;
  if (!sdk?.contacts) return undefined;
  const c = sdk.contacts;
  return {
    overview: (_spaceKey: string) => c.overview() as Promise<SpaceContactsDto>,
    updateProfile: (_spaceKey, rootId, patch) =>
      c.updateProfile(rootId, patch as Parameters<typeof c.updateProfile>[1]),
    setBlocked: (_spaceKey, rootId, blocked) => c.setBlocked(rootId, blocked),
    removeFriend: (rootId, block) => c.removeFriend(rootId, block),
    sendRequest: (input) => c.sendRequest(input),
    replyRequest: (requestId, text) => c.replyRequest(requestId, text),
    askRequest: (requestId, text) => c.askRequest(requestId, text),
    resolveRequest: (requestId, accept, permission) => c.resolveRequest(requestId, accept, permission),
    tagCreate: (_spaceKey, id, name) => c.tagCreate(id, name),
    tagRename: (_spaceKey, tagId, name) => c.tagRename(tagId, name),
    tagDelete: (_spaceKey, tagId) => c.tagDelete(tagId),
    groupCreate: (_spaceKey, id, name) => c.groupCreate(id, name),
    groupRename: (_spaceKey, groupId, name) => c.groupRename(groupId, name),
    groupDelete: (_spaceKey, groupId) => c.groupDelete(groupId),
    groupMove: (_spaceKey, groupId, toIndex) => c.groupMove(groupId, toIndex),
    setGroup: (_spaceKey, rootId, groupId) => c.setGroup(rootId, groupId),
    orgGroupCreate: (_spaceKey, parentId, id, name) => c.orgGroupCreate(parentId, id, name),
    orgGroupRename: (_spaceKey, id, name) => c.orgGroupRename(id, name),
    orgGroupDelete: (_spaceKey, id) => c.orgGroupDelete(id),
    orgGroupMove: (_spaceKey, id, toIndex, newParentId) => c.orgGroupMove(id, toIndex, newParentId)
  };
}

/**
 * 组织接口（邀请记录）：v1 未进 SDK 面（organization 域接口面变更待拍板，
 * A19 遗留）——恒 undefined，水合链路静默降级（面板仅 overview 数据）。
 */
export function organizationApi(): HostOrganizationApi | undefined {
  return undefined;
}

/** 我加入的组织列表（org-membership 数据源）：sdk.runtime.listMineOrganizations
 *  直通（`org:read` 基础权限）；未绑定时拒 Promise（调用方按失败保留旧缓存处理） */
export function listMineOrganizations(): Promise<OrgView[]> {
  const sdk = _sdk;
  if (!sdk) return Promise.reject(new Error('spark-contacts: SDK not bound'));
  return sdk.runtime.listMineOrganizations() as Promise<OrgView[]>;
}

/** 当前身份（current-user / profile-extra 数据源）：sdk.runtime.currentRoot
 *  直通（免权限基础调用；扩展字段 gender/region/signature 随返回值携带） */
export function currentRoot(): Promise<{
  unlocked: boolean;
  rootId: string | null;
  nickname?: string | null;
  avatar?: string | null;
  gender?: string | null;
  region?: string | null;
  signature?: string | null;
}> {
  const sdk = _sdk;
  if (!sdk) return Promise.reject(new Error('spark-contacts: SDK not bound'));
  return sdk.runtime.currentRoot();
}

// ------------------------------------------------------------------
// 事件面（桥事件 → 壳层 P2pEventDto 同形分发）
// ------------------------------------------------------------------

let eventsBound = false;

/**
 * 事件订阅适配：壳层 `listenP2pEvents(handler)` → SDK 通讯录事件面。
 * - ContactsSynced / OrgSynced → onChanged（轻量通知，store 重读 overview 收敛）；
 * - FriendRequestReceived/Sent/Accepted → onRequestChanged；
 * - FriendProfileUpdated → onFriendProfileUpdated；
 * 幂等（多次调用只订阅一次）。OrgInviteUpdated 属 organization 域 v1 缺口
 * （同 organizationApi，事件不到达，面板行为随之降级）。
 */
export function listenP2pEvents(handler: (event: P2pEventDto) => void): Promise<void> {
  const sdk = _sdk;
  if (!sdk?.contacts) return Promise.resolve();
  if (!eventsBound) {
    eventsBound = true;
    void sdk.contacts.onChanged(() => {
      // 与壳层同口径：ContactsSynced 整页重拉个人空间；OrgSynced 由各已缓存
      // org 空间重拉（store 的 OrgSynced 分支读 event.data.orgContacts，这里
      // 无法区分来源——统一按 ContactsSynced + OrgSynced 各发一次，两个分支
      // 幂等重拉，行为等价）
      handler({ kind: 'ContactsSynced', data: {} });
      handler({ kind: 'OrgSynced', data: { orgContacts: 1 } });
    });
    void sdk.contacts.onRequestChanged((e) => {
      handler({ kind: e.kind, data: { request: e.request } });
    });
    void sdk.contacts.onFriendProfileUpdated((e) => {
      handler({ kind: 'FriendProfileUpdated', data: e });
    });
  }
  return Promise.resolve();
}
