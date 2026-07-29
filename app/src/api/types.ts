/**
 * 宿主 API 类型定义（自 api/index.ts 拆出，纯结构移动）。
 *
 * 职责：ElectronAPI 接口形状与各域 DTO 类型，对齐旧 desktop/src/main/preload.ts；
 * 内联 PluginPermission/DomainSignature 的最小定义，避免跨进程目录引用。
 */

// ------------------------------------------------------------------
// P2P 事件（src-tauri 把内核 `P2pEvent` 结构化后以 `p2p-event` 全局事件转发；
// 线形为 serde 相邻标签 `{kind, data?}`，Lagged 由转发层合成）
// ------------------------------------------------------------------

/** P2P 事件载荷（与 spark-core `P2pEvent` 的 serde 形状一一对应）。 */
export type P2pEventDto =
  | { kind: 'Started'; data: { peerId: string; listenAddresses: string[] } }
  | { kind: 'ListenPortPersisted'; data: { port: number } }
  | { kind: 'PeerConnected'; data: { peerId: string } }
  | { kind: 'PeerDisconnected'; data: { peerId: string } }
  | { kind: 'PeerVersion'; data: { peerId: string; appVersion: string } }
  | { kind: 'AnnouncePublished'; data: { addresses: number } }
  | { kind: 'AnnounceAccepted'; data: { peerId: string } }
  | { kind: 'PeerExchangeCompleted'; data: { responder: string; merged: number } }
  | { kind: 'OrgShareAccepted'; data: { orgId: string; syncId: string | null; source: string } }
  | { kind: 'SyncMessageApplied'; data: { msgType: string; domain: string } }
  | { kind: 'MessageDropped'; data: { reason: string } }
  | { kind: 'KeepaliveTick'; data: { overlayDialed: number; exchanged: number; announced: boolean } }
  | { kind: 'ChatReceived'; data: { spaceKey: string; conversation: ConversationDto; message: ChatMessageDto } }
  | { kind: 'ChatStatus'; data: { spaceKey: string; convId: string; messageId?: string; status?: MessageStatusDto; recalled?: boolean; peerRead?: boolean } }
  | { kind: 'FriendRequestReceived'; data: { request: FriendRequestDto } }
  | { kind: 'FriendRequestAccepted'; data: { request: FriendRequestDto; friend: FriendDto } }
  | { kind: 'Warning'; data: string }
  | { kind: 'Stopped' }
  | { kind: 'Lagged'; skipped: number };

export type PluginPermission = string;
export type DomainSignature = {
  domain: string;
  domainId: string;
  publicKey: string;
  signature: string;
  payloadHash: string;
};

/** 插件目录项（静态目录 PLUGIN_CATALOG 的 DTO，api/index.ts）。 */
export type PluginCatalogItem = {
  id: string;
  domain: string;
  name: string;
  description: string;
  category: 'foundation' | 'business';
  version: string;
  views: string[];
  permissions?: string[];
  package?: {
    updateManifestUrl: string;
    signatureUrl: string;
    packageName: string;
    installCommand: string;
  };
};

/** RootID 状态（rootIdentity.status 返回，派生自 ElectronAPI，组件侧不再本地重复声明）。 */
export type RootStatusDto = Awaited<ReturnType<ElectronAPI['rootIdentity']['status']>>;

/** P2P 节点信息（p2p.info 返回，派生自 ElectronAPI；stores/network-status 与各组件共用）。 */
export type P2pInfoDto = Awaited<ReturnType<ElectronAPI['p2p']['info']>>;

export type DataUsageReportDto = {
  scannedAt: number;
  classes: Record<
    'documents' | 'indexes' | 'syncMeta' | 'evidence' | 'organization' | 'p2p' | 'system' | 'other',
    { keys: number; bytes: number }
  >;
  totalKeys: number;
  totalBytes: number;
  disk: { path: string; freeBytes: number; totalBytes: number; freeRatio: number } | null;
  warnings: { usageExceeded: boolean; diskLow: boolean };
};

// 插件市场线形（对齐旧 preload.ts pluginMarket 声明与 src-tauri market 模块 DTO）
export type PluginMarketItemDto = {
  id: string;
  domain: string;
  name: string;
  description: string;
  category: 'foundation' | 'business';
  version: string;
  views: string[];
  permissions: PluginPermission[];
  package: {
    updateManifestUrl: string;
    signatureUrl: string;
    packageName: string;
    installCommand: string;
  };
  installed: boolean;
  enabled: boolean;
  installedVersion: string | null;
  latestVersion: string | null;
  updateAvailable: boolean;
  lastCheckedAt: number | null;
  lastCheckReason: string;
};

export type PluginUpdateProbeDto = {
  pluginId: string;
  checkedAt: number;
  latestVersion: string | null;
  updateAvailable: boolean;
  reason: string;
};

export type InstalledPluginStateDto = {
  pluginId: string;
  version: string;
  packagePath: string;
  sha256: string;
  size: number;
  installedAt: number;
  enabled: boolean;
  grantedPermissions: PluginPermission[];
};

export type OrgView = {
  orgId: string;
  name: string;
  description: string;
  basePluginDomain?: string;
  createdAt: number;
  createdBy: string;
  updatedAt: number;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    joinedAt: number;
    addedBy: string;
    nodeInfo?: { peerId?: string; addresses: string[] };
  }>;
  currentUserRole: 'admin' | 'member' | null;
  isCurrentUserAdmin: boolean;
  memberCount: number;
  adminCount: number;
  gateways?: string[];
  orgAddress?: string;
  isPublic?: boolean;
  orgDisplayName?: string;
};

/** §16 组织地址记录（内核 OrgAddressRecord 的 camelCase 视图）。 */
export type OrgAddressRecordDto = {
  orgAddress: string;
  orgId: string;
  orgPublicKey: string;
  displayName?: string;
  gateways: string[];
  seq: number;
  publishedAt: number;
  ttl: number;
  signature: string;
};

/** 组织网络状态（core `OrgNetworkStatus::as_str`）。 */
export type OrgNetworkStatus = 'good' | 'unstable' | 'lost' | 'recovering' | 'localOnly';

/** 恢复模式状态（core `RecoveryState::as_str`）。 */
export type OrgRecoveryState = 'idle' | 'recovering' | 'failed';

export type OrgSyncOverviewDto = {
  orgId: string;
  replicaTarget: number;
  syncedPeers: number;
  totalMembers: number;
  members: Array<{
    rootId: string;
    peerId?: string;
    isSelf: boolean;
    everSynced: boolean;
    lastSyncedAt: number | null;
  }>;
  /** 已连接的组织成员节点数（不含本机；含本机副本数 = connectedPeers + 1）。 */
  connectedPeers: number;
  recoveryState: OrgRecoveryState;
  /** 恢复查询发起时间（idle 时为 null）。 */
  recoveryStartedAt: number | null;
  /** 最近一次与组织成员建立连接的时间（无记录为 null）。 */
  lastConnectedAt: number | null;
  dhtMode: 'off' | 'client' | 'server';
  status: OrgNetworkStatus;
};

// ------------------------------------------------------------------
// 通讯录 / 消息 DTO（与 src/mock/contacts/、src/mock/messages.ts
// 顶部类型逐字段对齐，camelCase 线形）
// ------------------------------------------------------------------

/** 朋友权限（开放 / 仅聊天），仅个人空间使用。 */
export type FriendPermissionDto = 'open' | 'chatOnly';

/** 联系人本地资料（备注/电话/标签/备忘/照片/拉黑，仅自己可见）。 */
export interface ContactProfileDto {
  remark: string;
  phones: string[];
  tagIds: string[];
  /** 所属分组：个人空间=ContactGroupDto.id，组织空间=树节点 id；'' = 未分组 */
  groupId: string;
  memo: string;
  photos: string[];
  permission: FriendPermissionDto;
  blocked: boolean;
}

/** 个人空间朋友。 */
export interface FriendDto extends ContactProfileDto {
  rootId: string;
  nickname: string;
  signature: string;
  gender?: 'male' | 'female';
  addedAt: number;
}

/** 朋友/成员申请。 */
export interface FriendRequestDto {
  id: string;
  rootId: string;
  nickname: string;
  message: string;
  source: string;
  status: 'pending' | 'accepted' | 'ignored' | 'replied' | 'failed';
  /** 申请发出/收到时间。 */
  createdAt?: number;
  /** 最近一次状态变化/新回复时间。 */
  updatedAt?: number;
  /** 有未看的新变化。 */
  unread?: boolean;
  /** 来回回复记录。 */
  thread?: Array<{ from: 'me' | 'peer'; text: string; ts: number }>;
  /** 组织邀请码（我发出的组织成员邀请）。 */
  inviteCode?: string;
}

/** 通讯录标签。 */
export interface ContactTagDto {
  id: string;
  name: string;
}

/** 个人空间分组（扁平一层，数组顺序即显示顺序）。 */
export interface ContactGroupDto {
  id: string;
  name: string;
}

/** 组织空间分组树节点（数组顺序即同级排序）。 */
export interface OrgGroupNodeDto {
  id: string;
  name: string;
  children: OrgGroupNodeDto[];
}

/** 单空间通讯录总览。 */
export interface SpaceContactsDto {
  friends: FriendDto[];
  requests: FriendRequestDto[];
  outgoing: FriendRequestDto[];
  tags: ContactTagDto[];
  groups: ContactGroupDto[];
  groupTree: OrgGroupNodeDto[];
  memberExtras: Record<string, ContactProfileDto>;
}

export type MessageTypeDto = 'text' | 'image' | 'file' | 'link' | 'voice' | 'system';
export type MessageStatusDto = 'sending' | 'sent' | 'delivered' | 'read' | 'failed';

/** 链接预览卡片。 */
export interface LinkPreviewDto {
  url: string;
  title: string;
  description: string;
  siteName: string;
  domain: string;
}

/** 引用回复携带的原消息片段。 */
export interface QuoteRefDto {
  messageId: string;
  senderName: string;
  preview: string;
}

export interface ChatMessageDto {
  id: string;
  senderId: string;
  senderName: string;
  type: MessageTypeDto;
  content: string;
  fileSize?: number;
  duration?: number;
  link?: LinkPreviewDto;
  quote?: QuoteRefDto;
  createdAt: number;
  /** 仅自己发送的消息有状态 */
  status?: MessageStatusDto;
  recalled: boolean;
}

export interface ConversationDto {
  id: string;
  kind: 'direct' | 'system';
  title: string;
  peerId: string;
  unreadCount: number;
  pinnedAt: number;
  muted: boolean;
  online: boolean;
  draft: string;
  updatedAt: number;
}

export type ElectronAPI = {
  db: {
    query: (prefix: string) => Promise<Array<{ key: string; value: string }>>;
  };
  evidence: {
    headHash: () => Promise<{ hash: string | null }>;
    verify: () => Promise<{ valid: boolean; height: number }>;
  };
  p2p: {
    start: () => Promise<{ started: boolean }>;
    stop: () => Promise<{ started: boolean }>;
    broadcast: (topic: string, message: unknown) => Promise<{ success: boolean }>;
    clearPeerRecords: () => Promise<{ cleared: number }>;
    syncPeerOrganizations: (targetPeer: { peerId?: string; addresses: string[] }) => Promise<{
      attempted: number; synced: number; pullChecked: number; pullSynced: number; removed: number;
    }>;
    info: () => Promise<{
      initialized: boolean; started: boolean; peerId: string | null; addresses: string[];
      connectedPeers: string[]; sparkSyncSubscribers: string[]; error?: string | null;
    }>;
    getDhtMode: () => Promise<{ dhtMode: 'off' | 'client' | 'server' }>;
    setDhtMode: (mode: 'off' | 'client' | 'server') => Promise<{ dhtMode: 'off' | 'client' | 'server' }>;
    makeNodeCard: (orgId?: string) => Promise<{ card: string }>;
    importNodeCard: (card: string) => Promise<{ peerId: string; hasRecoveryToken: boolean; connectError: string | null }>;
  };
  plugin: {
    openView: (pluginDomain: string, pluginView?: string) => Promise<{ success: boolean; windowId: number }>;
    listCatalog: () => Promise<PluginCatalogItem[]>;
    currentRoot: () => Promise<{ unlocked: boolean; rootId: string | null }>;
    identitySign: (payload: string, pluginDomain?: string) => Promise<DomainSignature>;
    identityVerify: (payload: string, signature: string, publicKey: string) => Promise<{ valid: boolean }>;
    syncOrganizationData: (orgId: string, pluginDomain?: string) => Promise<{ orgId: string; attempted: number; pulled: number }>;
    listMineOrganizations: (pluginDomain?: string) => Promise<OrgView[]>;
    docGet: <T extends Record<string, unknown> = Record<string, unknown>>(collection: string, id: string, pluginDomain?: string) => Promise<T | null>;
    docDeclareCollection: (
      collection: string,
      schema: { syncStrategy: 'append-only' | 'lww'; governance?: boolean; enableEvidence?: boolean },
      pluginDomain?: string
    ) => Promise<{
      collection: string;
      syncStrategy: 'append-only' | 'lww';
      governance: boolean;
      enableEvidence: boolean;
    }>;
    docPut: (collection: string, id: string, doc: Record<string, unknown>, pluginDomain?: string) => Promise<{ success: boolean }>;
    docDelete: (collection: string, id: string, pluginDomain?: string) => Promise<{ success: boolean }>;
    docQuery: <T extends Record<string, unknown> = Record<string, unknown>>(
      collection: string,
      options?: {
        limit?: number; reverse?: boolean;
        filter?: Array<{ field: string; value: string | number | boolean; op?: 'eq' | 'startsWith' | 'gt' | 'lt' | 'gte' | 'lte' }>;
      },
      pluginDomain?: string
    ) => Promise<{ items: Array<{ id: string; data: T }>; nextCursor?: string }>;
  };
  pluginMarket: {
    list: () => Promise<PluginMarketItemDto[]>;
    checkUpdates: (pluginId?: string) => Promise<PluginUpdateProbeDto[]>;
    install: (pluginId: string) => Promise<InstalledPluginStateDto>;
    upgrade: (pluginId: string) => Promise<InstalledPluginStateDto>;
    setEnabled: (pluginId: string, enabled: boolean) => Promise<InstalledPluginStateDto>;
  };
  organization: {
    listMine: () => Promise<OrgView[]>;
    create: (input: { name: string; description?: string; basePluginDomain?: string }) => Promise<OrgView>;
    delete: (orgId: string) => Promise<{ success: boolean }>;
    addMember: (orgId: string, input: { rootId: string; nodeInfo?: { peerId?: string; addresses: string[] } }) => Promise<OrgView>;
    removeMember: (orgId: string, memberRootId: string) => Promise<OrgView>;
    setGateways: (orgId: string, gateways: string[]) => Promise<OrgView>;
    createInvite: (orgId: string) => Promise<{ invite: string; orgId: string; orgName: string }>;
    acceptInvite: (code: string) => Promise<{ orgId: string; orgName: string; memberCount: number }>;
    getSyncOverview: (orgId: string) => Promise<OrgSyncOverviewDto | null>;
    setPublic: (orgId: string, isPublic: boolean, displayName?: string) => Promise<OrgView>;
    updateInfo: (orgId: string, patch: { name?: string; description?: string }) => Promise<OrgView>;
    resolveAddress: (orgAddress: string) => Promise<OrgAddressRecordDto | null>;
    searchKnown: (keyword: string) => Promise<OrgAddressRecordDto[]>;
  };
  contacts: {
    overview: (spaceKey: string) => Promise<SpaceContactsDto>;
    updateProfile: (spaceKey: string, rootId: string, patch: Partial<ContactProfileDto>) => Promise<{ success: boolean }>;
    setBlocked: (spaceKey: string, rootId: string, blocked: boolean) => Promise<{ success: boolean }>;
    removeFriend: (rootId: string) => Promise<{ success: boolean }>;
    sendRequest: (input: { id: string; rootId: string; raw: string; source: string; message: string }) => Promise<FriendRequestDto>;
    resolveRequest: (requestId: string, accept: boolean, permission: FriendPermissionDto) => Promise<{ success: boolean }>;
    tagCreate: (spaceKey: string, id: string, name: string) => Promise<ContactTagDto>;
    tagRename: (spaceKey: string, tagId: string, name: string) => Promise<{ success: boolean }>;
    tagDelete: (spaceKey: string, tagId: string) => Promise<{ success: boolean }>;
    groupCreate: (spaceKey: string, id: string, name: string) => Promise<ContactGroupDto>;
    groupRename: (spaceKey: string, groupId: string, name: string) => Promise<{ success: boolean }>;
    groupDelete: (spaceKey: string, groupId: string) => Promise<{ success: boolean }>;
    groupMove: (spaceKey: string, groupId: string, toIndex: number) => Promise<{ success: boolean }>;
    setGroup: (spaceKey: string, rootId: string, groupId: string) => Promise<{ success: boolean }>;
    orgGroupCreate: (spaceKey: string, parentId: string, id: string, name: string) => Promise<OrgGroupNodeDto | null>;
    orgGroupRename: (spaceKey: string, id: string, name: string) => Promise<{ success: boolean }>;
    orgGroupDelete: (spaceKey: string, id: string) => Promise<{ success: boolean }>;
    orgGroupMove: (spaceKey: string, id: string, toIndex: number) => Promise<{ success: boolean }>;
  };
  messages: {
    listConversations: (spaceKey: string) => Promise<ConversationDto[]>;
    listMessages: (spaceKey: string, convId: string) => Promise<ChatMessageDto[]>;
    ensureDirect: (spaceKey: string, peerId: string, title: string) => Promise<ConversationDto>;
    sendText: (spaceKey: string, convId: string, messageId: string, text: string, quote?: QuoteRefDto) => Promise<ChatMessageDto>;
    resend: (spaceKey: string, convId: string, messageId: string) => Promise<ChatMessageDto>;
    recall: (spaceKey: string, convId: string, messageId: string) => Promise<{ success: boolean }>;
    deleteMessage: (spaceKey: string, convId: string, messageId: string) => Promise<{ success: boolean }>;
    markRead: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
    setDraft: (spaceKey: string, convId: string, draft: string) => Promise<{ success: boolean }>;
    togglePin: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
    toggleMute: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
    clear: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
    deleteConversation: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  };
  rootIdentity: {
    status: () => Promise<{ initialized: boolean; unlocked: boolean; rootId: string | null; nickname: string | null; avatar: string | null }>;
    initialize: (password: string, nickname: string, avatar?: string | null) => Promise<{ rootId: string; mnemonic: string }>;
    unlock: (password: string, rootId?: string) => Promise<{ rootId: string }>;
    lock: () => Promise<{ success: boolean }>;
    sign: (payload: string) => Promise<{ rootId: string; signature: string; payloadHash: string }>;
    deriveDomain: (domain: string) => Promise<{ domain: string; domainId: string; publicKey: string; derivationPath: string }>;
    listIdentities: () => Promise<Array<{ rootId: string; createdAt: number; active: boolean; nickname: string | null; avatar: string | null }>>;
    setActive: (rootId: string) => Promise<{ success: boolean }>;
    updateProfile: (profile: { nickname?: string | null; avatar?: string | null }) => Promise<{ nickname: string | null; avatar: string | null }>;
    revealMnemonic: (password: string) => Promise<{ mnemonic: string }>;
    backupPayload: () => Promise<{ payload: string }>;
    checkMnemonic: (input: string) => Promise<{ words: string[]; invalidIndexes: number[] }>;
    recoverMnemonic: (mnemonic: string, newPassword: string, nickname: string, avatar?: string | null) => Promise<{ rootId: string }>;
    recoverBackup: (payload: string, password: string) => Promise<{ rootId: string }>;
  };
  updater: Record<string, (...args: never[]) => Promise<unknown>>;
  dataManagement: {
    usage: () => Promise<DataUsageReportDto>;
    cleanupNow: () => Promise<{ ranAt: number; tombstones: number; peerRecords: number; orgSyncStates: number }>;
    exportData: () => Promise<{ cancelled: true } | { cancelled: false; path: string; entries: number; bytes: number }>;
    purgePreview: (orgId: string, beforeTs: number) => Promise<unknown>;
    purgeExecute: (orgId: string, beforeTs: number, confirmExported: boolean) => Promise<unknown>;
  };
  getDomain: () => Promise<{ domain: string | null }>;
};
