/**
 * 宿主 API 适配层（Tauri 版）——组装与安装门面。
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
 *
 * 模块划分：类型在 ./types.ts，映射表在 ./command-map.ts，本文件保留
 * call/installHostApi/createTauriApi/listenP2pEvents 并 re-export 全部类型，
 * 既有 `from './api'` / `from '../api'` 引用路径不变。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import { ARG_NAMES, COMMAND_MAP } from './command-map';
import type { ElectronAPI, P2pEventDto, PluginCatalogItem } from './types';

// 类型门面：既有引用一律走 './api'，此处统一 re-export
export * from './types';

/** 订阅内核 P2P 事件流；返回取消订阅函数。 */
export function listenP2pEvents(handler: (event: P2pEventDto) => void): Promise<UnlistenFn> {
  return listen<P2pEventDto>('p2p-event', (event) => handler(event.payload));
}

/** 广播索引核查事件（announce_verify.rs 推渲染端的独立别名事件，载荷与 P2pEvent 相同）。 */
export type PluginAnnounceEvent =
  | { kind: 'received'; id: string; publisher: string }
  | { kind: 'verified'; id: string; verified: boolean; error: string | null };

/** 订阅广播索引 received/verified 事件；返回取消订阅函数（两路一起退订）。
 *  一路注册失败时退订已成功的另一路，避免残留半订阅状态。 */
export function listenPluginAnnounceEvents(
  handler: (event: PluginAnnounceEvent) => void
): Promise<UnlistenFn> {
  const received = listen<{ id: string; publisher: string }>('plugin-announce-received', (event) =>
    handler({ kind: 'received', ...event.payload })
  );
  const verified = listen<{ id: string; verified: boolean; error: string | null }>(
    'plugin-announce-verified',
    (event) => handler({ kind: 'verified', ...event.payload })
  );
  return Promise.all([received, verified]).then(
    ([unReceived, unVerified]) => () => {
      unReceived();
      unVerified();
    },
    async (error) => {
      // 部分失败：两路都尝试退订（已成功的一侧返回 unlisten，失败侧忽略）
      for (const pending of [received, verified]) {
        await pending.then((unlisten) => unlisten()).catch(() => {});
      }
      throw error;
    }
  );
}

/** .spkg 侧载文件选择（网络差降级导入入口）；用户取消返回 null。 */
export function pickSpkgFile(): Promise<string | null> {
  return openDialog({
    title: '导入 .spkg 插件包',
    filters: [{ name: 'Spark 插件包', extensions: ['spkg'] }]
  });
}

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
 * 插件域解析（命令层遗留回退）：
 * iframe 沙箱化后插件 SDK 调用一律经桥 dispatcher 显式带域（plugin-bridge-dispatcher.ts），
 * 此 URL query 回退仅保留给历史 tab 同进程语境，正常路径下解析不到 → null。
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
 * 经 plugin.listCatalog 下发给前端（组织与插件无绑定，不参与建组织）。
 * 重复字段（id/domain/name/version/views/package）派生自插件仓库内的
 * manifest.json（code/plugins/spark-example），消除双份维护；目录专属文案
 * （description）与市场侧字段（signatureUrl/installCommand/permissions 展示口径）
 * 仍在此保留。
 */
import sparkExampleManifest from '../../../plugins/spark-example/manifest.json';

const PLUGIN_CATALOG: PluginCatalogItem[] = [
  {
    id: sparkExampleManifest.id,
    domain: sparkExampleManifest.domain,
    name: sparkExampleManifest.name,
    description: '插件体系参考实现：管理员发帖（域签名防抵赖）发应用会话卡片通知，成员评论/回复。',
    category: 'foundation' as const,
    version: sparkExampleManifest.version,
    views: sparkExampleManifest.views.map((view) => view.id),
    permissions: ['storage:read', 'storage:write', 'org:read', 'org:sync', 'message:app', 'identity:sign'],
    package: {
      updateManifestUrl: sparkExampleManifest.package.updateManifestUrl,
      signatureUrl:
        'https://github.com/welyin/spark/releases/latest/download/spark-plugin-spark-example-manifest.sig',
      packageName: sparkExampleManifest.package.packageName,
      installCommand: `spark-plugin install ${sparkExampleManifest.package.packageName}`
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
      syncPeerOrganizations: (targetPeer) => call('p2p-sync-peer-organizations', targetPeer),
      // DHT 隐私开关：off=完全私有 / server=开放（默认）；client 为移动端预留
      getDhtMode: () => call('p2p-get-dht-mode'),
      setDhtMode: (mode) => call('p2p-set-dht-mode', mode),
      // 节点名片（org.md §17）：手动恢复连接的线下引导串；orgId 缺省=不附恢复 token
      makeNodeCard: (orgId?: string) => call('p2p-make-node-card', orgId ?? undefined),
      importNodeCard: (card: string) => call('p2p-import-node-card', card)
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
        call('plugin-market-set-enabled', pluginId, enabled),
      // 卸载：仅移除插件程序（包文件 + 状态记录），插件数据（文档/消息）保留在本机
      uninstall: (pluginId: string) => call('plugin-market-uninstall', pluginId),
      // 仓库锚定安装（plugin-dist）：先 resolveRepo 展示声明文件，确认后 installFromRepo
      resolveRepo: (id: string) => call('plugin-market-resolve-repo', id),
      installFromRepo: (id: string) => call('plugin-market-install-from-repo', id),
      // .spkg 侧载导入（网络差降级）：inspect 预览哈希供核对 → import 复核落状态
      inspectLocal: (path: string) => call('plugin-market-inspect-local', path),
      importLocal: (path: string, expectedSha256: string, confirmOverwrite = false) =>
        call('plugin-market-import-local', path, expectedSha256, confirmOverwrite),
      // 广播索引（plugin-dist §8）：开发者发布声明（签名+PoW+广播，秒级）与本地索引查询
      announcePublish: (input) => call('plugin-market-announce-publish', input),
      announceList: () => call('plugin-market-announce-list'),
      announceGet: (id: string) => call('plugin-market-announce-get', id)
    },
    organization: {
      listMine: () => call('org-list-mine'),
      create: (input) => call('org-create', input),
      delete: (orgId) => call('org-delete', orgId),
      addMember: (orgId, input) => call('org-add-member', orgId, input),
      removeMember: (orgId, memberRootId) => call('org-remove-member', orgId, memberRootId),
      setGateways: (orgId, gateways) => call('org-set-gateways', orgId, gateways),
      createInvite: (orgId) => call('org-invite-create', orgId),
      // 内核 accept_invite 已编排全段：解码邀请 → 连接邀请人 → claim 捎带 →
      // org-pull 拉取 → 成员确认（对齐 TS service.ts acceptOrgInvite）。
      acceptInvite: (code) => call('org-invite-accept', code),
      getSyncOverview: (orgId) => call('org-sync-overview', orgId),
      setPublic: (orgId, isPublic, displayName) =>
        call('org-set-public', orgId, isPublic, displayName ?? null),
      updateInfo: (orgId, patch) =>
        // 与 rootIdentity.updateProfile 同约定：字段缺省（undefined）= 不变，null = 明确清除；
        // avatar 空串 = 清除 logo（内核 settings.rs 口径）
        call('org-update-info', orgId, patch.name ?? undefined, patch.description ?? undefined, patch.avatar ?? undefined),
      // 成员更新自己的组织内身份（F2a）：缺省字段不传 = 不变；avatar null/'' = 清除
      // （B1：present-but-null 在 IPC 边界会被 serde 坍塌，故 null 归一为 '' 发送）；
      // gender/region/signature 空串 = 清除
      updateMyIdentity: (orgId, patch) =>
        call(
          'org-update-my-identity',
          orgId,
          patch.nickname ?? undefined,
          patch.avatar === undefined ? undefined : (patch.avatar ?? ''),
          patch.gender ?? undefined,
          patch.region ?? undefined,
          patch.signature ?? undefined,
          patch.usePersonalIdentity ?? undefined
        ),
      resolveAddress: (orgAddress) => call('org-resolve-address', orgAddress),
      searchKnown: (keyword) => call('org-search-known', keyword),
      // 组织邀请走 DM：管理员定向发送（寻址线索可省，内核按 显式参数→预录成员
      // nodeInfo→朋友记录 解析；都无线索报错）；被邀请人确认/拒绝幂等
      sendInvite: (input) =>
        call(
          'org-send-invite',
          input.orgId,
          input.targetRootId,
          input.targetPeerId ?? null,
          input.targetAddresses ?? null,
          input.targetNickname ?? null
        ),
      respondInvite: (input) => call('org-respond-invite', input.inviteId, input.accept),
      inviteRecords: (orgId) => call('org-invite-records', orgId)
    },
    contacts: {
      overview: (spaceKey) => call('contact-overview', spaceKey),
      updateProfile: (spaceKey, rootId, patch) => call('contact-update-profile', spaceKey, rootId, patch),
      setBlocked: (spaceKey, rootId, blocked) => call('contact-set-blocked', spaceKey, rootId, blocked),
      // block 缺省（undefined）= 只删不拉黑；true = §5.5 删除同时拉黑
      removeFriend: (rootId, block) => call('contact-remove-friend', rootId, block),
      sendRequest: (input) => call('contact-send-request', input),
      replyRequest: (requestId, text) => call('contact-reply-request', requestId, text),
      askRequest: (requestId, text) => call('contact-ask-request', requestId, text),
      resolveRequest: (requestId, accept, permission) =>
        call('contact-resolve-request', requestId, accept, permission),
      tagCreate: (spaceKey, id, name) => call('contact-tag-create', spaceKey, id, name),
      tagRename: (spaceKey, tagId, name) => call('contact-tag-rename', spaceKey, tagId, name),
      tagDelete: (spaceKey, tagId) => call('contact-tag-delete', spaceKey, tagId),
      groupCreate: (spaceKey, id, name) => call('contact-group-create', spaceKey, id, name),
      groupRename: (spaceKey, groupId, name) => call('contact-group-rename', spaceKey, groupId, name),
      groupDelete: (spaceKey, groupId) => call('contact-group-delete', spaceKey, groupId),
      groupMove: (spaceKey, groupId, toIndex) => call('contact-group-move', spaceKey, groupId, toIndex),
      setGroup: (spaceKey, rootId, groupId) => call('contact-set-group', spaceKey, rootId, groupId),
      orgGroupCreate: (spaceKey, parentId, id, name) => call('contact-org-group-create', spaceKey, parentId, id, name),
      orgGroupRename: (spaceKey, id, name) => call('contact-org-group-rename', spaceKey, id, name),
      orgGroupDelete: (spaceKey, id) => call('contact-org-group-delete', spaceKey, id),
      // newParentId 缺省（undefined）= 同级重排；'' = 移到根层（跨级移动）
      orgGroupMove: (spaceKey, id, toIndex, newParentId) => call('contact-org-group-move', spaceKey, id, toIndex, newParentId)
    },
    messages: {
      listConversations: (spaceKey) => call('message-list-conversations', spaceKey),
      listMessages: (spaceKey, convId) => call('message-list-messages', spaceKey, convId),
      ensureDirect: (spaceKey, peerId, title) => call('message-ensure-direct', spaceKey, peerId, title),
      sendText: (spaceKey, convId, messageId, text, quote) => call('message-send-text', spaceKey, convId, messageId, text, quote),
      resend: (spaceKey, convId, messageId) => call('message-resend', spaceKey, convId, messageId),
      recall: (spaceKey, convId, messageId) => call('message-recall', spaceKey, convId, messageId),
      deleteMessage: (spaceKey, convId, messageId) => call('message-delete', spaceKey, convId, messageId),
      markRead: (spaceKey, convId) => call('message-mark-read', spaceKey, convId),
      setDraft: (spaceKey, convId, draft) => call('message-set-draft', spaceKey, convId, draft),
      togglePin: (spaceKey, convId) => call('message-toggle-pin', spaceKey, convId),
      toggleMute: (spaceKey, convId) => call('message-toggle-mute', spaceKey, convId),
      clear: (spaceKey, convId) => call('message-clear', spaceKey, convId),
      deleteConversation: (spaceKey, convId) => call('message-delete-conversation', spaceKey, convId),
      // 应用消息（服务号模型，p2p-messages.md §20；SDK messages 域经桥注入 pluginId 调用）
      appSend: (spaceKey, pluginId, payload, card) => call('message-app-send', spaceKey, pluginId, payload, card),
      appList: (spaceKey, pluginId) => call('message-app-list', spaceKey, pluginId),
      appMarkRead: (spaceKey, pluginId) => call('message-app-mark-read', spaceKey, pluginId),
      appDeleteConversation: (spaceKey, pluginId) => call('message-app-delete-conversation', spaceKey, pluginId)
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
        // profile 对象，命令侧按字段可选传递（avatar: null/'' = 清除恢复自动头像；
        // 字段缺省 = 不变）。B1：present-but-null 在 IPC 边界会被 serde 坍塌为
        // 「缺省」，故 null 统一归一为 '' 发送（命令层 Some("") = 清除）。
        // 扩展字段性别/地区/签名：缺省/null = 不变，'' = 清除。
        invoke('root_update_profile', {
          nickname: profile.nickname ?? undefined,
          avatar: profile.avatar === undefined ? undefined : (profile.avatar ?? ''),
          gender: profile.gender ?? undefined,
          region: profile.region ?? undefined,
          signature: profile.signature ?? undefined
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
    system: {
      // 未读角标 → 系统徽标（F4）：macOS dock 角标 / Linux 任务栏计数，
      // 平台不支持时命令侧静默降级（见 src-tauri commands/system.rs）
      setBadge: (count) => call('system-set-badge', count)
    },
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
