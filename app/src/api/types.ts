/**
 * 宿主 API 类型定义（自 api/index.ts 拆出，纯结构移动）。
 *
 * 职责：ElectronAPI 接口形状与各域 DTO 类型，对齐旧 desktop/src/main/preload.ts；
 * 内联 PluginPermission/DomainSignature 的最小定义，避免跨进程目录引用。
 */

// ------------------------------------------------------------------
// 主程序自动更新（src-tauri commands/updater.rs；tauri-plugin-updater
// + GitHub Releases 的 latest.json 清单，minisign 验签）
// ------------------------------------------------------------------

/** 更新状态（TestPage 调试面板字段对齐；`staged` = 已下载待重启安装的包） */
export interface UpdaterStatusDto {
  configured: boolean;
  appId: string;
  channel: string;
  currentVersion: string;
  lastCheck?: { checkedAt: number; reason: string; availableVersion?: string } | null;
  staged?: { fileName: string; version: string } | null;
}

/** 手动检查结果（`availableVersion` 仅在有更新时存在） */
export interface UpdaterCheckResultDto {
  updateAvailable: boolean;
  availableVersion?: string;
  notes?: string;
  date?: string;
}

/** 已下载待安装的包描述 */
export interface UpdaterStagedDto {
  fileName: string;
  version: string;
}

/** `updater://ready` 事件载荷（后台自动检查+下载就绪，等待用户确认重启） */
export interface UpdaterReadyInfo {
  version: string;
  notes?: string;
  date?: string;
}

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
  | { kind: 'FeedReceived'; data: FeedMessageDto }
  | { kind: 'ChatStatus'; data: { spaceKey: string; convId: string; messageId?: string; status?: MessageStatusDto; recalled?: boolean; peerRead?: boolean } }
  | { kind: 'FriendRequestReceived'; data: { request: FriendRequestDto } }
  | { kind: 'FriendRequestSent'; data: { request: FriendRequestDto } }
  | { kind: 'FriendRequestAccepted'; data: { request: FriendRequestDto; friend: FriendDto } }
  | { kind: 'FriendProfileUpdated'; data: { rootId: string; nickname: string; avatar?: string } }
  | { kind: 'SelfProfileSynced'; data: { nickname: string; avatar?: string } }
  | { kind: 'ContactsSynced'; data: { applied: number } }
  | { kind: 'OrgSynced'; data: { orgMeta: number; orgContacts: number } }
  | { kind: 'PluginDataChanged'; data: { pluginId: string; name: string; keys: string[] } }
  | { kind: 'ConversationsSynced'; data: { applied: number } }
  | { kind: 'DeviceUpdated'; data: DeviceDto }
  // M1 新设备通知（m1-m2-implementation-plan §3.3）：kind 恒 'device_joined'，
  // deviceId 为新设备 peerId，ts 为通知发出时间（ms）
  | { kind: 'DeviceNoticeReceived'; data: { kind: string; deviceId: string; deviceName: string; ts: number } }
  // M5 延迟恢复：state 只发 initiated / vetoed / committed；ready 由 root_recovery_status.readyToConfirm 体现
  | {
      kind: 'RecoveryUpdated';
      data: {
        requestId: string;
        fromDevice: string;
        state: 'initiated' | 'vetoed' | 'committed';
        op?: 'reset_password' | 'pair_new_device';
        deadline?: number;
      };
    }
  // 乙+校验器：密码被其他设备改密/重置，需要本机统一到新密码
  | { kind: 'PasswordChangeObserved'; data: { rotatedAt: number; rotatedBy: string; rotatedByDevice: string; reason: 'password_change' | 'password_reset' } }
  // 乙+校验器：所有设备都已完成统一，撤提示
  | { kind: 'PasswordUnificationDone'; data: { rotatedAt: number } }
  // D'：本机已超出统一密码的宽限期
  | { kind: 'DeviceOutOfGrace'; data: { passwordChangedAt: number; graceMs: number } }
  | { kind: 'OrgInviteReceived'; data: OrgInviteRecordDto }
  | { kind: 'OrgInviteUpdated'; data: OrgInviteRecordDto }
  | { kind: 'Warning'; data: string }
  | { kind: 'Stopped' }
  | { kind: 'Lagged'; skipped: number };

// 乙+校验器
export interface PasswordVerifyTicketResultDto {
  ok: boolean;
  rotatedAt?: number;
  rotatedByDevice?: string;
}

export interface PasswordUnifyStatusDto {
  pending: boolean;
  rotatedAt?: number;
  rotatedByDevice?: string;
  reason?: 'password_change' | 'password_reset';
}

export interface PasswordUnifyResultDto {
  success: boolean;
}

export type PluginPermission = string;
/** 插件可运行的空间类型（spaces-and-plugins §4）。 */
export type PluginSpaceType = 'personal' | 'org';
export type DomainSignature = {
  domain: string;
  domainId: string;
  publicKey: string;
  signature: string;
  payloadHash: string;
};

/** 插件目录项（市场列表条目 catalog 部分的 DTO；仓库锚定合成条目来源）。 */
/** 插件运行时前提（与 SDK PluginRequires 对齐） */
export type PluginRequires = {
  /** 需要的系统能力子集（permissions 的超集校验） */
  capabilities?: string[];
  /** 明确限定平台（缺省 = 全平台） */
  platforms?: Array<'desktop' | 'mobile'>;
  /** 移动端只读豁免：声明后移动端可安装但禁用写能力 */
  mobileReadonly?: boolean;
};

export type PluginCatalogItem = {
  id: string;
  domain: string;
  name: string;
  description: string;
  category: 'ai-assistant' | 'social' | 'tool' | 'game' | 'foundation';
  version: string;
  views: string[];
  permissions?: string[];
  /** 插件支持的空间类型；缺省按 ['org'] 处理（spaces-and-plugins §4） */
  supportedSpaces?: PluginSpaceType[];
  /** 运行时前提（平台/能力约束；缺省 = 全平台可装） */
  requires?: PluginRequires;
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

/** 设备清单项（devices.list 返回，派生自 ElectronAPI；设备管理页数据源）。 */
export type DeviceDto = Awaited<ReturnType<ElectronAPI['devices']['list']>>[number];

/** 设备撤销结果（M2 `root_revoke_device`；内核编排完成后恒 success: true，失败走 reject）。 */
export type DeviceRevokeResult = { success: boolean };

/**
 * 安全日志条目（security:log:{ts}:{kind}:{deviceId} 前缀 KV；`security_log_list`
 * 内部调试命令返回）。形状对齐壳层 dto.rs SecurityLogEntryDto（camelCase 序列化）；
 * deviceName/actor 仅 initiated 事件记录（🟠4），其余事件缺省。
 */
export interface SecurityLogEntryDto {
  key: string;
  kind: string;
  deviceId: string;
  deviceName?: string;
  actor?: string;
  ts: number;
}

/** `security-log-list` 出参（决策点 3：本期仅收敛点，无 UI）。 */
export interface SecurityLogListResult {
  items: SecurityLogEntryDto[];
}

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
  category: 'ai-assistant' | 'social' | 'tool' | 'game' | 'foundation';
  version: string;
  views: string[];
  permissions: PluginPermission[];
  /** 插件支持的空间类型；缺省按 ['org'] 处理（spaces-and-plugins §4） */
  supportedSpaces?: PluginSpaceType[];
  /** 运行时前提（平台/能力约束；缺省 = 全平台可装） */
  requires?: PluginRequires;
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
  /** 已授权权限清单（桥 dispatcher 权限中间件数据源；未安装时为空） */
  grantedPermissions: PluginPermission[];
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
  /** 信任层级：'signed' | 'repo-anchored'（仓库锚定，plugin-dist §4.2）| 'sideloaded'（.spkg 侧载）；缺省 = 签名信任链 */
  trust?: string;
};

/** .spkg 侧载预览（plugin-market-inspect-local 出参；网络差降级，波次 2b） */
export type SideloadPreviewDto = {
  pluginId: string;
  domain: string;
  version: string;
  name: string;
  /** 包内 manifest.json 声明的权限（已规范化） */
  permissions: string[];
  /** 包内 manifest.json 声明的支持空间（已规范化；缺省 = 未声明，按 ['org'] 口径） */
  supportedSpaces?: PluginSpaceType[];
  /** 整包 sha256（确认对话框展示供核对；import 复核） */
  sha256: string;
  size: number;
  fileName: string;
};

/** 仓库声明文件 spark-plugin.json（plugin-dist §2；resolveRepo 出参，id 已规范化） */
export type RepoPluginDeclarationDto = {
  id: string;
  name: string;
  icon: string;
  summary: string;
  category: string;
  version: string;
  releaseAssetPattern: string;
  permissions: string[];
  mirrors: string[];
  /** 插件支持的空间类型（可选；缺省按 ['org'] 处理，§2.1） */
  supportedSpaces?: PluginSpaceType[];
  sdkVersion: string;
};

/** 广播索引声明消息（plugin-dist §8.2；发布输入为其子集） */
export type PluginAnnounceInputDto = {
  id: string;
  name: string;
  icon: string;
  summary: string;
  category: string;
  version: string;
  releaseUrl: string;
};

/** 广播索引完整声明消息（plugin-dist §8.2） */
export type PluginAnnounceDto = PluginAnnounceInputDto & {
  type: string;
  timestamp: number;
  ttl: number;
  publisher: string;
  pubKey: string;
  pow: { bits: number; nonce: number };
  signature: string;
};

/** 懒惰核查校正后的展示字段（plugin-dist §8.8：以仓库声明文件为准；announce 自报值仅作占位） */
export type CorrectedAnnounceFieldsDto = {
  name: string;
  icon: string;
  summary: string;
  version: string;
  /** 声明文件的 supportedSpaces（可选；缺省按 ['org'] 处理） */
  supportedSpaces?: PluginSpaceType[];
  /** supportedSpaces 已核查标记（一次性迁移用；旧索引条目缺席 = 待重核，前端不消费） */
  supportedSpacesChecked?: boolean;
};

/** 广播索引本地索引条目（plugin-dist §8.7；verified 只有 verified 态进市场视图） */
export type PluginAnnounceIndexEntryDto = {
  announce: PluginAnnounceDto;
  firstSeenAt: number;
  updatedAt: number;
  verified: 'pending' | 'verified' | 'failed';
  verifyError: string;
  verifiedAt: number;
  /** 核查通过时回写的校正展示字段；同 id 新声明到达时重置缺席 */
  corrected?: CorrectedAnnounceFieldsDto;
};

/** 组织成员设备端点（按 deviceUid 聚合的端点集之一；deviceUid 缺省 = 旧声明）。 */
export type OrgNodeInfo = {
  deviceUid?: string;
  peerId?: string;
  addresses: string[];
};

export type OrgView = {
  orgId: string;
  name: string;
  description: string;
  /** 组织 logo（data URL）；可能缺省/空串 */
  avatar?: string;
  basePluginDomain?: string;
  createdAt: number;
  createdBy: string;
  updatedAt: number;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    joinedAt: number;
    addedBy: string;
    // 端点化：单设备退化为 `{deviceUid?, peerId?, addresses}` 对象，多设备为数组
    nodeInfo?: OrgNodeInfo | OrgNodeInfo[];
    // 组织身份字段（F2a）：仅本人可改，未设置时键不出现
    nickname?: string;
    avatar?: string;
    signature?: string;
    gender?: string;
    region?: string;
    /** true = 组织内展示个人身份；缺省键不出现（视为 false） */
    usePersonalIdentity?: boolean;
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

// ------------------------------------------------------------------
// M5 延迟恢复 DTO（内核 recovery 模块 camelCase 序列化）
// ------------------------------------------------------------------

/** 恢复操作类型。 */
export type RecoveryOpDto = 'reset_password' | 'pair_new_device';

/** 恢复请求状态（只发 initiated / vetoed / committed）。 */
export type RecoveryStateDto = 'initiated' | 'vetoed' | 'committed';

/** 待确认/已结束的恢复请求记录。 */
export interface RecoveryPendingDto {
  requestId: string;
  op: RecoveryOpDto;
  /** 发起方：本机 pending 的确认截止时间；接收方 initiated：本地否决窗终点。 */
  deadline: number;
  initiatedAt: number;
  vetoed: boolean;
  state: RecoveryStateDto;
}

/** `root_recovery_status` 出参。 */
export interface RecoveryStatusDto {
  pending: RecoveryPendingDto | null;
  /** 本机作为接收方且已到达可确认窗口（需结合 pending.state === 'initiated' 使用）。 */
  readyToConfirm: boolean;
}

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
  /** O1 两级记账：逐数据账号的 PC 设备达标状态（org-data-sync §4）。 */
  dataAccounts: Array<{
    rootId: string;
    pcSynced: boolean;
    deviceClass: string;
  }>;
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
  /** 对端同步过来的头像（data URL）；缺省走自动头像 */
  avatar?: string;
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
  /** 最近一次状态变化/新回复时间（毫秒；内核必填，前端映射仍保留缺省兜底）。 */
  updatedAt: number;
  /** 有未看的新变化。 */
  unread?: boolean;
  /** 来回回复记录。 */
  thread?: Array<{ from: 'me' | 'peer'; text: string; ts: number }>;
  /** 组织邀请码（我发出的组织成员邀请）。 */
  inviteCode?: string;
  /** 对端同步过来的头像（data URL）；缺省走自动头像 */
  avatar?: string;
}

/** 组织邀请记录（内核 org/invite_record.rs serde camelCase 直出；出/入站共用）。 */
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
  /** 邀请码（仅 incoming 记录携带，供重启后仍能加入） */
  inviteCode?: string;
  createdAt: number;
  updatedAt: number;
};

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

/**
 * 朋友只读摘要（社交投递层 social-feed §9.4 `contact:read` 最小只读面）。
 * 相对 `FriendDto` 裁剪：剔除签名/性别/电话/备忘/照片/设备寻址等敏感字段，
 * 仅暴露插件「谁可以看」选择器所需的展示与筛选字段。
 */
export interface FriendSummaryDto {
  rootId: string;
  nickname: string;
  /** 无头像时缺省键不出现（对齐 FriendDto.avatar） */
  avatar?: string;
  /** 所属分组 id；'' = 未分组 */
  groupId: string;
  tagIds: string[];
  permission: FriendPermissionDto;
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
// 'streaming'：AI 流式回复中间态（内核 bot_reply_stream_* 落 status='streaming'，
// 终态 delivered/failed）；serde 直传字符串，DTO 与内核 MessageRecord.status 对齐
export type MessageStatusDto = 'sending' | 'sent' | 'delivered' | 'read' | 'failed' | 'streaming';

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
  // §20 应用会话（app:{pluginId}）以 kind='app' 到达，peerId 占位填 pluginId；
  // mock/messages.ts 的 Conversation 已同步加宽
  kind: 'direct' | 'system' | 'app';
  title: string;
  peerId: string;
  unreadCount: number;
  pinnedAt: number;
  muted: boolean;
  online: boolean;
  draft: string;
  updatedAt: number;
}

/** 应用消息卡片（message-card 富渲染视图，p2p-messages.md §20.2）。 */
export interface AppMessageCardDto {
  viewId: string;
  data?: unknown;
}

/** 应用消息（服务号模型；本地生成、本地消费，状态恒 'local'，无 delivered 语义）。 */
export interface AppMessageDto {
  id: string;
  pluginId: string;
  /** 纯文本摘要（trim 后的 payload.summary；未装插件时壳层原生渲染此字段） */
  summary: string;
  /** 插件自描述 JSON（必须含非空 summary 字段，否则内核拒绝写入） */
  payload: Record<string, unknown>;
  card?: AppMessageCardDto;
  createdAt: number;
  status: 'local';
  read: boolean;
}

// ------------------------------------------------------------------
// M4 生物识别
// ------------------------------------------------------------------

export interface BiometricStatusDto {
  available: boolean;
  enrolled: boolean;
  hasSecret: boolean;
}

export interface BiometricUnlockResultDto {
  rootId: string;
  password: string;
}

// ------------------------------------------------------------------
// 社交定向投递（social-feed §9，sdk.feed 域：deliver/pull/onReceive）
// ------------------------------------------------------------------

/** 单条 feed 消息（收件箱 pull / 在线推送 onReceive 共用形状）。 */
export interface FeedMessageDto {
  feedId: string;
  from: string;
  topic: string;
  /** 业务 payload（明文） */
  payload: unknown;
  /** 回执语义（指向原 feedId）；可省 */
  replyTo?: string;
  /** 信封时间戳（ms） */
  ts: number;
}

/** `sdk.feed.deliver` 返回（聚合计数；被静默跳过的收件人不计入 accepted）。 */
export type FeedDeliverResultDto = {
  requested: number;
  accepted: number;
};

/** `sdk.feed.pull` 返回（收件箱游标补读分页）。 */
export type FeedPullResultDto = {
  items: FeedMessageDto[];
  nextCursor?: string;
};


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
    /** 网络接口变化通知（WiFi↔蜂窝切换）：内核异步 debounce 后重发布地址，无结果回传 */
    networkChanged: () => Promise<void>;
  };
  plugin: {
    openView: (pluginDomain: string, pluginView?: string) => Promise<{ success: boolean; windowId: number }>;
    currentRoot: () => Promise<{
      unlocked: boolean;
      rootId: string | null;
      /** 当前身份昵称（root-status IdentityStatus 透传；未设置为 null） */
      nickname?: string | null;
      /** 当前身份头像 data URL（root-status IdentityStatus 透传；无头像为 null） */
      avatar?: string | null;
      gender?: string | null;
      region?: string | null;
      signature?: string | null;
    }>;
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
    // P6 声明式数据 API（wiki design/plugin-data-api.md）
    dataDeclareCollection: (
      declaration: {
        name: string;
        version?: string;
        scope?: 'sync' | 'local';
        devices?: 'all' | 'pc-backup' | 'pc-only' | 'mobile-only';
        merge?: 'lww-record' | 'append-only' | 'whole';
        // F8：org space 声明携带 orgId（桥按插件实例绑定注入，防插件自报任意组织）
        orgId?: string;
      },
      pluginDomain?: string
    ) => Promise<Record<string, unknown>>;
    // O3 读/写 org 上下文：orgId 由内核按插件实例所属空间解析（插件 API 零同步参数），
    // iframe 桥按 identity.space.id 注入；personal space 传 null。
    dataSave: (name: string, key: string, value: unknown, version?: string, orgId?: string, pluginDomain?: string) => Promise<{ success: boolean }>;
    dataDelete: (name: string, key: string, version?: string, orgId?: string, pluginDomain?: string) => Promise<{ success: boolean }>;
    dataGet: <T = unknown>(name: string, key: string, version?: string, orgId?: string, pluginDomain?: string) => Promise<T | null>;
    dataQuery: <T = unknown>(
      name: string,
      options?: { prefix?: string; limit?: number; cursor?: string },
      version?: string,
      orgId?: string,
      pluginDomain?: string
    ) => Promise<{ items: Array<{ key: string; value: T }>; nextCursor?: string }>;
    dataDropVersion: (name: string, version: string, pluginDomain?: string) => Promise<{ success: boolean }>;
    dataSaveBlob: (dataBase64: string) => Promise<{ hash: string; size: number }>;
    dataReadBlob: (hash: string) => Promise<{ status: 'ready'; data: string } | { status: 'pending' }>;
    /** O4 encrypted 授权名单（owner 侧）：orgId 由桥绑定注入 */
    dataGrantAccess: (
      orgId: string,
      name: string,
      version: string,
      members: string[]
    ) => Promise<{ owners: string[]; readers: string[]; epoch: number }>;
    dataRevokeAccess: (
      orgId: string,
      name: string,
      version: string,
      members: string[]
    ) => Promise<{ owners: string[]; readers: string[]; epoch: number }>;
    dataListAccess: (
      orgId: string,
      name: string,
      version: string
    ) => Promise<{ owners: string[]; readers: string[]; epoch: number }>;
  };
  pluginMarket: {
    list: () => Promise<PluginMarketItemDto[]>;
    checkUpdates: (pluginId?: string) => Promise<PluginUpdateProbeDto[]>;
    upgrade: (pluginId: string) => Promise<InstalledPluginStateDto>;
    setEnabled: (pluginId: string, enabled: boolean) => Promise<InstalledPluginStateDto>;
    /** 卸载：移除状态记录并删除包文件；插件数据（文档/消息）保留在本机 */
    uninstall: (pluginId: string) => Promise<void>;
    /** 仓库锚定安装前置解析：拉取并校验 spark-plugin.json（plugin-dist §4.1） */
    resolveRepo: (id: string) => Promise<RepoPluginDeclarationDto>;
    /** 仓库锚定安装（plugin-dist §4.2） */
    installFromRepo: (id: string) => Promise<InstalledPluginStateDto>;
    /** .spkg 侧载预览：解析容器 + 整包哈希（网络差降级，波次 2b） */
    inspectLocal: (path: string) => Promise<SideloadPreviewDto>;
    /** .spkg 侧载导入：复核整包哈希 → 逐文件校验 → 落状态（trust = 'sideloaded'）；
     *  覆盖既有更高信任安装需 confirmOverwrite = true（后端报确认前缀后重试） */
    importLocal: (path: string, expectedSha256: string, confirmOverwrite?: boolean) => Promise<InstalledPluginStateDto>;
    /** 发布插件声明（plugin-dist §8，开发者模式：签名 + PoW + 广播；秒级） */
    announcePublish: (input: PluginAnnounceInputDto) => Promise<PluginAnnounceIndexEntryDto>;
    /** 本地广播索引列表（含 verified 状态；市场视图只展示 verified，波次 2b） */
    announceList: () => Promise<PluginAnnounceIndexEntryDto[]>;
    /** 单条广播索引查询（verified 状态） */
    announceGet: (id: string) => Promise<PluginAnnounceIndexEntryDto | null>;
  };
  pluginRuntime: {
    /** 插件后台运行时对账（幂等）：登录/身份切换进入主界面时调用，
     *  按当前身份拉起已启用插件的 QuickJS 后台线程 */
    syncBackgrounds: () => Promise<void>;
    /** 宿主 → 插件后台反向查询；插件未运行/超时（2s）返回 null */
    hostQuery: (pluginId: string, kind: string, payload: unknown) => Promise<unknown>;
    /** 插件后台运行时是否存活（bot 在线状态的权威来源） */
    isBackgroundRunning: (pluginId: string) => Promise<boolean>;
  };
  organization: {
    listMine: () => Promise<OrgView[]>;
    create: (input: { name: string; description?: string; avatar?: string; basePluginDomain?: string }) => Promise<OrgView>;
    delete: (orgId: string) => Promise<{ success: boolean }>;
    addMember: (orgId: string, input: { rootId: string; nodeInfo?: OrgNodeInfo }) => Promise<OrgView>;
    removeMember: (orgId: string, memberRootId: string) => Promise<OrgView>;
    setGateways: (orgId: string, gateways: string[]) => Promise<OrgView>;
    /** O1：指定数据账号（空数组 = 清除显式指定、回落缺省全体管理员） */
    setDataAccounts: (orgId: string, dataAccounts: string[]) => Promise<OrgView>;
    /** O1：晋升/降级成员角色（数据职责随角色自动进出） */
    setMemberRole: (orgId: string, memberRootId: string, role: 'admin' | 'member') => Promise<OrgView>;
    createInvite: (orgId: string) => Promise<{ invite: string; orgId: string; orgName: string }>;
    acceptInvite: (code: string) => Promise<{ orgId: string; orgName: string; memberCount: number }>;
    getSyncOverview: (orgId: string) => Promise<OrgSyncOverviewDto | null>;
    setPublic: (orgId: string, isPublic: boolean, displayName?: string) => Promise<OrgView>;
    updateInfo: (orgId: string, patch: { name?: string; description?: string; avatar?: string }) => Promise<OrgView>;
    /**
     * 成员更新自己的组织内身份（F2a `org_update_my_identity`）：
     * 字段缺省（undefined）= 不变；avatar null/'' = 清除（B1：IPC 边界 null 会坍塌，
     * 适配层统一归一为 '' 发送，与 gender/region/signature 的空串清除同口径）；
     * gender/region/signature 空串 = 清除；nickname 不可清除（内核校验 1–24 字符）。
     */
    updateMyIdentity: (
      orgId: string,
      patch: {
        nickname?: string;
        avatar?: string | null;
        gender?: string;
        region?: string;
        signature?: string;
        usePersonalIdentity?: boolean;
      }
    ) => Promise<OrgView>;
    resolveAddress: (orgAddress: string) => Promise<OrgAddressRecordDto | null>;
    searchKnown: (keyword: string) => Promise<OrgAddressRecordDto[]>;
    /** 组织邀请走 DM：仅管理员；寻址 显式参数 → 预录成员 nodeInfo → 朋友记录 */
    sendInvite: (input: {
      orgId: string;
      targetRootId: string;
      targetPeerId?: string | null;
      targetAddresses?: string[] | null;
      targetNickname?: string | null;
    }) => Promise<OrgInviteRecordDto>;
    /** 被邀请人确认/拒绝（幂等；accept 先执行加入编排，成功才落 accepted） */
    respondInvite: (input: { inviteId: string; accept: boolean }) => Promise<OrgInviteRecordDto>;
    /** 某组织的邀请记录（出/入站合并） */
    inviteRecords: (orgId: string) => Promise<OrgInviteRecordDto[]>;
  };
  contacts: {
    overview: (spaceKey: string) => Promise<SpaceContactsDto>;
    // 只读门面（社交投递层 contact:read，插件 SDK contacts 模块）
    listFriends: () => Promise<FriendSummaryDto[]>;
    listGroups: () => Promise<ContactGroupDto[]>;
    listTags: () => Promise<ContactTagDto[]>;
    updateProfile: (spaceKey: string, rootId: string, patch: Partial<ContactProfileDto>) => Promise<{ success: boolean }>;
    setBlocked: (spaceKey: string, rootId: string, blocked: boolean) => Promise<{ success: boolean }>;
    removeFriend: (rootId: string, block?: boolean) => Promise<{ success: boolean }>;
    sendRequest: (input: { id: string; rootId: string; raw: string; peerId?: string; addresses?: string[]; source: string; message: string }) => Promise<FriendRequestDto>;
    replyRequest: (requestId: string, text: string) => Promise<FriendRequestDto>;
    askRequest: (requestId: string, text: string) => Promise<FriendRequestDto>;
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
    orgGroupMove: (spaceKey: string, id: string, toIndex: number, newParentId?: string) => Promise<{ success: boolean }>;
  };
  feed: {
    /** 社交定向投递（social-feed §9.1 deliver）。pluginId 由桥绑定的插件身份注入（不信插件自报）；权限/限流在桥 dispatcher 强制 */
    deliver: (
      pluginId: string,
      topic: string,
      payload: unknown,
      recipients: string[],
      replyTo?: string,
      feedId?: string
    ) => Promise<FeedDeliverResultDto>;
    /** 收件箱游标补读（§9.1 pull；接收侧免权限） */
    pull: (
      pluginId: string,
      topic: string,
      cursor?: string,
      limit?: number
    ) => Promise<FeedPullResultDto>;
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
    // 应用消息（服务号模型，p2p-messages.md §20）：payload 必须含非空 summary；
    // 每插件每会话限流 10 条/分钟，超限 reject rate-limited
    appSend: (spaceKey: string, pluginId: string, payload: Record<string, unknown>, card?: AppMessageCardDto) => Promise<AppMessageDto>;
    appList: (spaceKey: string, pluginId: string) => Promise<AppMessageDto[]>;
    appMarkRead: (spaceKey: string, pluginId: string) => Promise<{ success: boolean }>;
    appDeleteConversation: (spaceKey: string, pluginId: string) => Promise<{ success: boolean }>;
  };
  rootIdentity: {
    status: () => Promise<{
      initialized: boolean; unlocked: boolean; rootId: string | null; nickname: string | null; avatar: string | null;
      // 扩展字段：None 序列化为 null（无 serde skip），故类型为 `| null`（与 .vue 初始字面量兼容）
      gender?: string | null; region?: string | null; signature?: string | null;
    }>;
    initialize: (password: string, nickname: string, avatar?: string | null) => Promise<{ rootId: string; mnemonic: string }>;
    unlock: (password: string, rootId?: string) => Promise<{ rootId: string }>;
    lock: () => Promise<{ success: boolean }>;
    sign: (payload: string) => Promise<{ rootId: string; signature: string; payloadHash: string }>;
    deriveDomain: (domain: string) => Promise<{ domain: string; domainId: string; publicKey: string; derivationPath: string }>;
    listIdentities: () => Promise<Array<{ rootId: string; createdAt: number; active: boolean; nickname: string | null; avatar: string | null; gender?: string | null; region?: string | null; signature?: string | null }>>;
    setActive: (rootId: string) => Promise<{ success: boolean }>;
    // 扩展字段性别/地区/签名：undefined/null = 不变，'' = 清除，其余 = 设置（与内核 patch 语义对齐）；
    // avatar：undefined = 不变，null/'' = 清除（B1：null 由适配层归一为 '' 发送）
    updateProfile: (profile: {
      nickname?: string | null; avatar?: string | null;
      gender?: string | null; region?: string | null; signature?: string | null;
    }) => Promise<{
      nickname: string | null; avatar: string | null;
      gender: string | null; region: string | null; signature: string | null;
    }>;
    revealMnemonic: (password: string) => Promise<{ mnemonic: string }>;
    /** 修改登录密码：必须验证当前密码（device-trust-and-biometric §2 高危操作验密码，不做免密通道） */
    changePassword: (oldPassword: string, newPassword: string) => Promise<{ success: boolean }>;
    backupPayload: () => Promise<{ payload: string }>;
    /** 二维码备份载荷（验密；剔除头像等大字段的紧凑 JSON，适配 QR 容量上限） */
    backupPayloadQr: (password: string) => Promise<{ payload: string }>;
    checkMnemonic: (input: string) => Promise<{ words: string[]; invalidIndexes: number[] }>;
    recoverMnemonic: (mnemonic: string, newPassword: string, nickname: string, avatar?: string | null) => Promise<{ rootId: string }>;
    recoverBackup: (payload: string, password: string) => Promise<{ rootId: string }>;
  };
  biometric: {
    check: () => Promise<BiometricStatusDto>;
    unlock: () => Promise<BiometricUnlockResultDto>;
    storePassword: (password: string) => Promise<{ success: boolean }>;
    delete: () => Promise<{ success: boolean }>;
  };
  /** 主程序自动更新（tauri-plugin-updater + GitHub Releases 清单；src-tauri commands/updater.rs） */
  updater: {
    status: () => Promise<UpdaterStatusDto>;
    check: () => Promise<UpdaterCheckResultDto>;
    stageLatest: () => Promise<UpdaterStagedDto>;
    applyRestart: () => Promise<void>;
    /** 后台自动检查完成且更新已下载就绪（验签通过）：订阅重启确认弹窗（返回退订函数） */
    onReady: (cb: (info: UpdaterReadyInfo) => void) => Promise<() => void>;
  };
  // M5 延迟恢复（多设备间密码重置/配对新设备的安全窗口协议）
  recovery: {
    status: () => Promise<RecoveryStatusDto>;
    initiate: (op: RecoveryOpDto, delayHours?: number) => Promise<RecoveryPendingDto>;
    confirm: (requestId: string, newPassword: string) => Promise<RecoveryPendingDto>;
    veto: (requestId: string) => Promise<void>;
  };
  passwordUnify: {
    verifyTicket: (password: string) => Promise<PasswordVerifyTicketResultDto>;
    unifyPassword: (oldPassword: string, newPassword: string) => Promise<PasswordUnifyResultDto>;
    status: () => Promise<PasswordUnifyStatusDto>;
  };
  devices: {
    /** 设备清单：本机置顶（isSelf），其余按最近在线证据降序 */
    list: () => Promise<Array<{
      peerId: string; deviceName: string; os: string; osVersion: string; arch: string; macs: string[];
      appVersion: string; updatedAt: number; lastSeenAt: number; isSelf: boolean; online: boolean;
      /** M2 撤销时间（ms）；未撤销为 null/缺省（老版本记录无此字段） */
      revokedAt?: number | null;
    }>>;
    /** M2 撤销设备：授权集移除 + 连接层黑名单断连（m1-m2-implementation-plan §4.3） */
    revoke: (deviceId: string) => Promise<DeviceRevokeResult>;
    /** 安全日志（内部调试命令，决策点 3 本期不做 UI）；limit 可选透传 */
    securityLogList: (limit?: number) => Promise<SecurityLogListResult>;
  };
  sys: {
    exec: (program: string, args: string[], workdir?: string) => Promise<{ stdout: string; stderr: string; exitCode: number }>;
    fetch: (url: string, options?: { method?: string; headers?: Record<string, string>; body?: string }) => Promise<{ status: number; headers: Record<string, string>; body: string }>;
    fetchStream: (url: string, options?: { method?: string; headers?: Record<string, string>; body?: string }) => Promise<{ streamId: string }>;
    /** 目录选择对话框（纯前端 tauri-plugin-dialog）；用户取消返回 null */
    pickFolder: (title?: string) => Promise<string | null>;
  };
  system: {
    /** 未读角标 → 系统徽标（dock/任务栏）；平台不支持时命令侧静默，始终 resolve */
    setBadge: (count: number) => Promise<void>;
    /** 当前 HTTP 代理（"host:port"），未设置返回 null */
    getProxy: () => Promise<string | null>;
    /** 设置 HTTP 代理（host:port，空串关闭）；已建立的连接需重启应用后生效 */
    setProxy: (proxy: string) => Promise<void>;
    /** 移动端返回键在一级页（栈底）时显式退出应用（原生默认动作已被 JS 监听拦截） */
    exitApp: () => Promise<void>;
  };

  dataManagement: {
    usage: () => Promise<DataUsageReportDto>;
    cleanupNow: () => Promise<{ ranAt: number; tombstones: number; peerRecords: number; orgSyncStates: number }>;
    exportData: () => Promise<{ cancelled: true } | { cancelled: false; path: string; entries: number; bytes: number }>;
    purgePreview: (orgId: string, beforeTs: number) => Promise<PurgePreviewDto>;
    purgeExecute: (orgId: string, beforeTs: number, confirmExported: boolean) => Promise<PurgeResultDto>;
  };
  getDomain: () => Promise<{ domain: string | null }>;
};

export type PurgePreviewDto = {
  orgId: string;
  domain: string;
  beforeTs: number;
  preview: { collections: string[]; affectedDocs: number; affectedBytes: number };
  replica: OrgSyncOverviewDto | null;
  isCurrentUserAdmin: boolean;
};

export type PurgeResultDto = {
  domain: string;
  beforeTs: number;
  collections: string[];
  removedDocs: number;
  freedBytes: number;
};
