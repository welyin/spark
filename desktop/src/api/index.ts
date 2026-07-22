/**
 * 宿主 API 适配层（Tauri 版）。
 *
 * 设计要点：
 * - 页面组件只认 `window.electronAPI`（旧 Electron preload 暴露的形状）。
 *   本模块在 Tauri 环境下用 `@tauri-apps/api` 的 invoke 实现**完全相同**的接口，
 *   页面零改动；非 Tauri 环境（旧 Electron / 单测）不覆盖既有 `electronAPI`。
 * - Electron 的 `ipcRenderer.invoke(channel, ...args)` 是"位置参数"；
 *   Tauri 的 `invoke(command, args)` 是"命名参数对象"。映射规则：
 *   `COMMAND_MAP` 把 channel（kebab-case）映射为 command（snake_case），
 *   `ARG_NAMES` 按位置给出参数名（camelCase —— Tauri 自动映射到 Rust 的
 *   snake_case 形参）。形状特殊（如 plugin.doc* 需要注入 domain）的走手写包装。
 * - 未实现的通道：`todo()` 生成明确报错的桩，错误信息含通道名，便于排查。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { save as saveDialog } from '@tauri-apps/plugin-dialog';

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
  | { kind: 'Warning'; data: string }
  | { kind: 'Stopped' }
  | { kind: 'Lagged'; skipped: number };

/** 订阅内核 P2P 事件流；返回取消订阅函数。 */
export function listenP2pEvents(handler: (event: P2pEventDto) => void): Promise<UnlistenFn> {
  return listen<P2pEventDto>('p2p-event', (event) => handler(event.payload));
}

// ------------------------------------------------------------------
// 类型（对齐旧 desktop/src/main/preload.ts 的 ElectronAPI；
// 内联 PluginPermission/DomainSignature 的最小定义，避免跨进程目录引用）
// ------------------------------------------------------------------

export type PluginPermission = string;
export type DomainSignature = {
  domain: string;
  domainId: string;
  publicKey: string;
  signature: string;
  payloadHash: string;
};

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
};

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
  };
  plugin: {
    openView: (pluginDomain: string, pluginView?: string) => Promise<{ success: boolean; windowId: number }>;
    listCatalog: () => Promise<unknown[]>;
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
    create: (input: { name: string; description?: string; basePluginDomain: string }) => Promise<OrgView>;
    delete: (orgId: string) => Promise<{ success: boolean }>;
    addMember: (orgId: string, input: { rootId: string; nodeInfo?: { peerId?: string; addresses: string[] } }) => Promise<OrgView>;
    removeMember: (orgId: string, memberRootId: string) => Promise<OrgView>;
    createInvite: (orgId: string) => Promise<{ invite: string; orgId: string; orgName: string }>;
    acceptInvite: (code: string) => Promise<{ orgId: string; orgName: string; memberCount: number }>;
    getSyncOverview: (orgId: string) => Promise<OrgSyncOverviewDto | null>;
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

// ------------------------------------------------------------------
// channel → command 映射
// ------------------------------------------------------------------

/** Electron 通道名 → Tauri command 名（snake_case）。 */
const COMMAND_MAP: Record<string, string> = {
  // 身份
  'root-status': 'root_status',
  'root-init': 'root_init',
  'root-unlock': 'root_unlock',
  'root-lock': 'root_lock',
  'root-list-identities': 'root_list_identities',
  'root-set-active': 'root_set_active',
  'root-recover-mnemonic': 'root_recover_mnemonic',
  'root-recover-backup': 'root_recover_backup',
  'root-backup-payload': 'root_backup_payload',
  'root-reveal-mnemonic': 'root_reveal_mnemonic',
  'root-update-profile': 'root_update_profile',
  'root-sign': 'root_sign',
  'root-derive-domain': 'root_derive_domain',
  'root-mnemonic-check': 'root_mnemonic_check',
  // 文档（plugin.doc* 手写包装，不走通用表）
  // 组织
  'org-list-mine': 'org_list_mine',
  'org-create': 'org_create',
  'org-invite-create': 'org_create_invite',
  'org-invite-accept': 'org_accept_invite',
  'org-sync-overview': 'org_sync_overview',
  'org-delete': 'org_delete',
  'org-add-member': 'org_add_member',
  'org-remove-member': 'org_remove_member',
  // 数据治理
  'data-usage': 'data_usage',
  'data-cleanup-now': 'data_cleanup_now',
  'data-export': 'data_export',
  'data-purge-preview': 'data_purge_preview',
  'data-purge-execute': 'data_purge_execute',
  // 存证
  'evidence-head-hash': 'evidence_head_hash',
  'evidence-verify': 'evidence_verify',
  // P2P
  'p2p-start': 'p2p_start',
  'p2p-stop': 'p2p_stop',
  'p2p-info': 'p2p_status',
  'p2p-broadcast': 'p2p_broadcast',
  'p2p-clear-peer-records': 'p2p_clear_peer_records',
  'p2p-sync-peer-organizations': 'p2p_sync_peer_organizations',
  'p2p-list-peer-records': 'p2p_list_peer_records',
  // 插件运行时（tab 模式语义，见下方注记）
  'plugin-identity-sign': 'plugin_identity_sign',
  'plugin-identity-verify': 'plugin_identity_verify',
  'plugin-org-sync-now': 'plugin_org_sync_now',
  // 插件市场
  'plugin-market-list': 'plugin_market_list',
  'plugin-market-check-updates': 'plugin_market_check_updates',
  'plugin-market-install': 'plugin_market_install',
  'plugin-market-upgrade': 'plugin_market_upgrade',
  'plugin-market-set-enabled': 'plugin_market_set_enabled'
};

/**
 * 各通道的位置参数名（camelCase；Tauri 将其映射到 Rust snake_case 形参）。
 * 缺省为 []（无参通道）。undefined 值会被 JSON 序列化丢弃 → Rust 侧得 None。
 */
const ARG_NAMES: Record<string, string[]> = {
  'root-init': ['password', 'nickname', 'avatar'],
  'root-unlock': ['password', 'rootId'],
  'root-set-active': ['rootId'],
  'root-recover-mnemonic': ['mnemonic', 'newPassword', 'nickname', 'avatar'],
  'root-recover-backup': ['payload', 'password'],
  'root-reveal-mnemonic': ['password'],
  'root-sign': ['payload'],
  'root-derive-domain': ['domain'],
  'root-mnemonic-check': ['input'],
  'org-create': ['input'],
  'org-invite-create': ['orgId'],
  'org-invite-accept': ['code'],
  'org-sync-overview': ['orgId'],
  'org-delete': ['orgId'],
  'org-add-member': ['orgId', 'input'],
  'org-remove-member': ['orgId', 'memberRootId'],
  'data-export': ['filePath'],
  'data-purge-preview': ['orgId', 'beforeTs'],
  'data-purge-execute': ['orgId', 'beforeTs', 'confirmExported'],
  'p2p-broadcast': ['topic', 'message'],
  'p2p-sync-peer-organizations': ['targetPeer'],
  'plugin-identity-sign': ['payload', 'pluginDomain'],
  'plugin-identity-verify': ['payload', 'signature', 'publicKey'],
  'plugin-org-sync-now': ['orgId', 'pluginDomain'],
  'plugin-market-check-updates': ['pluginId'],
  'plugin-market-install': ['pluginId'],
  'plugin-market-upgrade': ['pluginId'],
  'plugin-market-set-enabled': ['pluginId', 'enabled']
};

/** 通用调用：channel + 位置参数 → command + 命名参数。 */
async function call<T>(channel: string, ...args: unknown[]): Promise<T> {
  const command = COMMAND_MAP[channel] ?? channel;
  const names = ARG_NAMES[channel] ?? [];
  const payload: Record<string, unknown> = {};
  names.forEach((name, index) => {
    payload[name] = args[index];
  });
  return invoke<T>(command, payload);
}

/** 未实现通道的桩：报清楚的错误而不是静默 undefined。 */
// 返回类型用 any[] 形参：桩需可赋到任意签名位置（strict 下 never[] 不可赋）
function todo(channel: string): (...args: any[]) => Promise<never> {
  return () =>
    Promise.reject(
      new Error(`[tauri-shell] "${channel}" 尚未在 Tauri 壳中实现（参见 src/api/index.ts TODO 清单）`)
    );
}

/**
 * 插件域解析（本期沿用旧 tab 模式语义）：
 * 插件视图跑在 system 域窗口的 iframe tab 内，URL query 带 `pluginDomain`
 * （App.vue `pluginFrameSrc` 注入）；主窗口无该参数 → null。
 * 独立插件窗口由宿主绑定域 + 强制权限校验待插件运行时排期。
 */
function resolveTabPluginDomain(): string | null {
  if (typeof window === 'undefined') {
    return null;
  }
  const fromQuery = new URLSearchParams(window.location.search).get('pluginDomain')?.trim() ?? '';
  if (!fromQuery.startsWith('plugin:') || fromQuery.length <= 'plugin:'.length) {
    return null;
  }
  return fromQuery;
}

/**
 * plugin.* 系列命令的域实参：显式 pluginDomain 优先，缺省回退 tab URL query。
 * 都解析不到说明调用发生在非插件上下文（主窗口系统域），报清楚的错误。
 */
function requireDomain(pluginDomain: string | undefined): string {
  const domain = pluginDomain ?? resolveTabPluginDomain();
  if (!domain) {
    throw new Error('[tauri-shell] pluginDomain 缺失：非插件 tab 上下文，无法解析插件域');
  }
  return domain;
}

/**
 * 插件目录静态清单（vendored 自 TS main/plugins/catalog.ts）。
 * 插件运行时（安装/验签/独立窗口）本期不在壳范围，目录本身是纯静态数据，
 * OrgPage 建组织时依赖它解析 basePluginDomain。
 */
const PLUGIN_CATALOG = [
  {
    id: 'weibo-core',
    domain: 'plugin:weibo-core',
    name: '组织微博基础插件',
    description: '单主管理员发帖，组织成员评论/回复，基于插件域独立数据同步。',
    category: 'foundation' as const,
    version: '0.1.0',
    views: ['default'],
    permissions: ['org:sync'],
    package: {
      updateManifestUrl:
        'https://github.com/welyin/spark/releases/latest/download/spark-plugin-weibo-core-manifest.json',
      signatureUrl:
        'https://github.com/welyin/spark/releases/latest/download/spark-plugin-weibo-core-manifest.sig',
      packageName: 'spark-plugin-weibo-core-0.1.0.spkg',
      installCommand: 'spark-plugin install spark-plugin-weibo-core-0.1.0.spkg'
    }
  }
];

/** TS db.query 在测试页的唯一活用法：邻居活跃度记录前缀。 */
const PEER_RECORD_PREFIX = 'p2p:peer:record:';

// ------------------------------------------------------------------
// 组装与安装
// ------------------------------------------------------------------

/** 构造与 window.electronAPI 完全同形的 Tauri 实现。 */
export function createTauriApi(): ElectronAPI {
  return {
    db: {
      // TestPage 邻居列表的唯一活用法（p2p:peer:record: 前缀）映射到内核
      // 专用命令，返回同样的 { key, value }[] 形状；其余前缀保持未实现报错
      query: (prefix: string) =>
        prefix === PEER_RECORD_PREFIX ? call('p2p-list-peer-records') : todo('db-query')()
    },
    evidence: {
      headHash: () => call('evidence-head-hash'),
      verify: () => call('evidence-verify')
    },
    p2p: {
      start: () => call('p2p-start'),
      stop: () => call('p2p-stop'),
      info: () => call('p2p-info'),
      broadcast: (topic, message) => call('p2p-broadcast', topic, message),
      clearPeerRecords: () => call('p2p-clear-peer-records'),
      // 定向反熵对账：内核编排（双向 stale 推送 + org-pull + removed 清理），
      // 返回形状对齐 TS { attempted, synced, pullChecked, pullSynced, removed, skipped }
      syncPeerOrganizations: (targetPeer) => call('p2p-sync-peer-organizations', targetPeer)
    },
    plugin: {
      // TODO: 插件独立窗口属于插件运行时，本期不在壳范围（插件走 tab 模式）
      openView: todo('plugin-open-view'),
      // 静态目录（见 PLUGIN_CATALOG）：对齐 TS，每次调用返回深拷贝
      listCatalog: async () => structuredClone(PLUGIN_CATALOG),
      currentRoot: async () => {
        const status = await call<{ unlocked: boolean; rootId: string | null }>('root-status');
        return { unlocked: status.unlocked, rootId: status.rootId };
      },
      // 以下三个命令本期沿用旧 tab 模式语义：插件在 system 域运行、高级权限
      // 不做强制校验，域一律显式传给命令（缺省回退 tab URL query，见
      // requireDomain）。命令侧语义对齐旧 ipc/plugin.ts，见 src-tauri
      // commands/plugin.rs 注记。
      identitySign: (payload, pluginDomain) =>
        call('plugin-identity-sign', payload, requireDomain(pluginDomain)),
      identityVerify: (payload, signature, publicKey) =>
        call('plugin-identity-verify', payload, signature, publicKey),
      syncOrganizationData: (orgId, pluginDomain) =>
        call('plugin-org-sync-now', orgId, requireDomain(pluginDomain)),
      listMineOrganizations: () => call('org-list-mine'),
      docGet: (collection, id, pluginDomain) =>
        invoke('doc_get', { domain: requireDomain(pluginDomain), collection, id }),
      docDeclareCollection: (collection, schema, pluginDomain) =>
        invoke('doc_declare_collection', {
          domain: requireDomain(pluginDomain),
          collection,
          declaration: schema
        }),
      docPut: (collection, id, doc, pluginDomain) =>
        invoke('doc_put', { domain: requireDomain(pluginDomain), collection, id, doc }),
      docDelete: (collection, id, pluginDomain) =>
        invoke('doc_delete', { domain: requireDomain(pluginDomain), collection, id }),
      docQuery: (collection, options = {}, pluginDomain) =>
        invoke('doc_query', { domain: requireDomain(pluginDomain), collection, options })
    },
    pluginMarket: {
      // 市场服务在 src-tauri market 模块（验签/下载/落状态/对账）；
      // 形状对齐旧 preload，AppsPage 零改动
      list: () => call('plugin-market-list'),
      checkUpdates: (pluginId?: string) => call('plugin-market-check-updates', pluginId),
      install: (pluginId: string) => call('plugin-market-install', pluginId),
      upgrade: (pluginId: string) => call('plugin-market-upgrade', pluginId),
      setEnabled: (pluginId: string, enabled: boolean) =>
        call('plugin-market-set-enabled', pluginId, enabled)
    },
    organization: {
      listMine: () => call('org-list-mine'),
      create: (input) => call('org-create', input),
      delete: (orgId) => call('org-delete', orgId),
      addMember: (orgId, input) => call('org-add-member', orgId, input),
      removeMember: (orgId, memberRootId) => call('org-remove-member', orgId, memberRootId),
      createInvite: (orgId) => call('org-invite-create', orgId),
      // 内核 accept_invite 已编排全段：解码邀请 → 连接邀请人 → claim 捎带 →
      // org-pull 拉取 → 成员确认（对齐 TS service.ts acceptOrgInvite）。
      acceptInvite: (code) => call('org-invite-accept', code),
      getSyncOverview: (orgId) => call('org-sync-overview', orgId)
    },
    rootIdentity: {
      status: () => call('root-status'),
      initialize: (password, nickname, avatar) => call('root-init', password, nickname, avatar ?? null),
      unlock: (password, rootId) => call('root-unlock', password, rootId),
      lock: () => call('root-lock'),
      sign: (payload) => call('root-sign', payload),
      deriveDomain: (domain) => call('root-derive-domain', domain),
      listIdentities: () => call('root-list-identities'),
      setActive: (rootId) => call('root-set-active', rootId),
      updateProfile: (profile) =>
        // TS 主进程为免密码会话语义（root-id.ts updateProfile）；内核以 unlock 会话
        // 缓存口令重封加密 payload（spec §5），语义对齐。形状抹平：preload 传单个
        // profile 对象，命令侧按字段可选传递（avatar: null = 清除恢复自动头像；
        // 字段缺省 = 不变）。
        invoke('root_update_profile', {
          nickname: profile.nickname ?? undefined,
          avatar: profile.avatar === undefined ? undefined : profile.avatar
        }),
      revealMnemonic: (password) => call('root-reveal-mnemonic', password),
      backupPayload: () => call('root-backup-payload'),
      checkMnemonic: (input) => call('root-mnemonic-check', input),
      recoverMnemonic: (mnemonic, newPassword, nickname, avatar) =>
        call('root-recover-mnemonic', mnemonic, newPassword, nickname, avatar ?? null),
      recoverBackup: (payload, password) => call('root-recover-backup', payload, password)
    },
    updater: new Proxy({} as ElectronAPI['updater'], {
      // TODO: 更新器为 Electron 专属流程；Tauri 版应改用 tauri-plugin-updater
      get: (_, prop) => todo(`update-${String(prop)}`)
    }),
    dataManagement: {
      usage: () => call('data-usage'),
      cleanupNow: () => call('data-cleanup-now'),
      exportData: async () => {
        // TS 主进程流程（ipc/data.ts:57-71）：保存对话框取路径 → 写导出文件；
        // 对话框走 tauri-plugin-dialog，写文件仍由 data_export 命令在内核完成。
        const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
        const filePath = await saveDialog({
          title: '导出数据',
          defaultPath: `spark-export-${stamp}.json`,
          filters: [{ name: 'JSON', extensions: ['json'] }]
        });
        if (!filePath) {
          return { cancelled: true as const };
        }
        const result = await call<{ path: string; entries: number; bytes: number }>(
          'data-export',
          filePath
        );
        return { cancelled: false as const, ...result };
      },
      purgePreview: (orgId, beforeTs) => call('data-purge-preview', orgId, beforeTs),
      purgeExecute: (orgId, beforeTs, confirmExported) =>
        call('data-purge-execute', orgId, beforeTs, confirmExported)
    },
    // 域解析沿用旧 tab 模式语义：插件 iframe tab 从 URL query 取 pluginDomain；
    // 主窗口（系统域）无该参数 → null。独立插件窗口的宿主绑定域待插件运行时排期。
    getDomain: () => Promise.resolve({ domain: resolveTabPluginDomain() })
  };
}

/** 是否为 Tauri 运行环境。 */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && !!(window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
}

/**
 * 应用启动时调用：Tauri 环境下把 `window.electronAPI` 安装为 invoke 实现。
 * 非 Tauri 环境（旧 Electron/单测已有 electronAPI）不覆盖。
 */
export function installHostApi(): void {
  if (isTauri()) {
    (window as unknown as { electronAPI: ElectronAPI }).electronAPI = createTauriApi();
  }
}
