/**
 * 桥调用 dispatcher + 权限中间件（设计文档「权限模型」运行时强制）。
 *
 * createBridgeHost 的 handler 工厂：把桥 call（module/method/args）分发到
 * ./sdk-browser.ts 的后端实现（createPluginBackend，域一律显式下传；
 * messages 域走 ./messages 壳层服务，pluginId/space 由桥按绑定身份注入），
 * 每次调用前做三重过滤：
 *
 * 1) grantedPermissions：读市场安装状态（pluginMarket.list 聚合的
 *    grantedPermissions，内核侧持久化，渲染进程不可自报）；读取失败按空清单
 *    （最小授权，仅免权限基础调用放行）；
 * 2) view type 裁剪：app 主视图全量、message-card 仅 docs/data 只读、验签类
 *    与 affairs 只读（VIEW_ALLOWED_CALLS 映射表，后续按 view 扩充）；
 * 3) 当前 space：manifest supportedSpaces 不含当前 space 类型时整域拒绝；
 *    org 域调用（runtime.syncOrganizationData/listMineOrganizations、
 *    org.listMine）在 personal 空间下一律拒绝；org 空间下
 *    syncOrganizationData 的 org 实参必须与当前 space 一致。
 *
 * 未授权一律抛 `Access denied: ...`（与 TS 旧权限中间件文案同前缀）。
 *
 * identity:sign 为「使用时询问」高危权限：首次调用经 ElMessageBox 确认，
 * 按 插件 ID+域名 记忆本次会话决定（会话级，不落盘）；并发首调复用同一确认。
 *
 * 身份三元组（pluginId/viewId/space）来自 createBridgeHost 绑定的值，
 * 不信插件自报（hello 自报仅做一致性核对，见 bridge/host.ts）。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ElMessageBox } from 'element-plus';
import type { PluginCredentialsAPI, PluginMarketAPI, PluginSDK, PluginSpaceContext } from '../../../packages/plugin-sdk/src';
import type { BridgeHostHandler } from '../../../packages/plugin-sdk/src/bridge/host';
import { createPluginBackend } from './sdk-browser';
import { listAppMessages, markAppMessagesRead, sendAppMessage } from './messages';
import type { AppMessageCardDto, ElectronAPI } from '../api/types';
import { refreshContacts, ensurePluginContactTag } from '../mock/contacts';
import { OPEN_PLUGIN_DEEPLINK_EVENT } from '../services/deep-link';
import { saveFileWithDialog } from '../api';

/** 桥事件泵：由外部（PluginIframeHost）注入，用于将 Tauri 事件转发为桥 event。 */
export interface BridgeEventPump {
  pushEvent: (event: string, payload?: unknown) => void;
}

let _eventPump: BridgeEventPump | null = null;

/** 注入事件泵（仅 PluginIframeHost 调用一次）。 */
export function setBridgeEventPump(pump: BridgeEventPump | null) {
  _eventPump = pump;
}

type PluginViewType = 'app' | 'message-card' | 'background';

/** 调用 → 所需权限；不在表内 = 免权限基础调用（验签纯函数、运行时状态读取等） */
const CALL_PERMISSIONS: Record<string, string> = {
  'docs.get': 'storage:read',
  'docs.query': 'storage:read',
  'docs.defineCollection': 'storage:write',
  'docs.put': 'storage:write',
  'docs.delete': 'storage:write',
  // P6 声明式数据 API（与内核 host_env.rs capability_permission 逐字对齐）
  'data.get': 'storage:read',
  'data.query': 'storage:read',
  'data.readBlob': 'storage:read',
  'data.declareCollection': 'storage:write',
  'data.save': 'storage:write',
  'data.delete': 'storage:write',
  'data.dropVersion': 'storage:write',
  'data.saveBlob': 'storage:write',
  'runtime.listMineOrganizations': 'org:read',
  'runtime.syncOrganizationData': 'org:sync',
  'p2p.broadcast': 'network:broadcast',
  'identity.sign': 'identity:sign',
  // 应用会话读写（高级权限 + 内核限流 10 条/60s，§20.5）
  'messages.sendAppMessage': 'message:app',
  'messages.listAppMessages': 'message:app',
  'messages.markRead': 'message:app',
  // sys 代理（内核外呼）：高危操作，每个方法独立授权
  'sys.exec': 'system:exec',
  'sys.fetch': 'network:fetch',
  'sys.fetchStream': 'network:fetch',
  'sys.pickFolder': 'system:exec',
  // 壳层代存文件（保存对话框为用户主动行为，路径由用户选定）归 storage:read，
  // 与 org.exportData（org:read）/ market.pickSpkg（market:read）壳层对话框先例同口径
  'sys.saveFile': 'storage:read',
  // 插件联系人消息方法（统一在 messages 命名空间下）
  'messages.registerAsContact': 'message:app',
  'messages.unregisterAsContact': 'message:app',
  'messages.sendResponse': 'message:app',
  // 通讯录只读门面（社交投递层 §9.4 contact:read；高级 + 使用时询问）
  'contacts.listFriends': 'contact:read',
  'contacts.listGroups': 'contact:read',
  'contacts.listTags': 'contact:read',
  // 社交定向投递（social-feed §9.3 + A18 §4.1 权限归一：deliver=feed:write
  // 高级 + 内核限流；pull/订阅收件=feed:read）
  'feed.deliver': 'feed:write',
  'feed.pull': 'feed:read',
  // A18 插件数据 API 面（communication §4.1）：IM 数据面 messages:read/write
  // （均高危确认）；space 由桥绑定注入
  'messages.conversations': 'messages:read',
  'messages.list': 'messages:read',
  'messages.send': 'messages:write',
  'messages.recall': 'messages:write',
  'messages.markConversationRead': 'messages:write',
  // A19 写面补全（等语义移植；聊天应用迁移缺口）
  'messages.ensureDirect': 'messages:write',
  'messages.resend': 'messages:write',
  'messages.deleteMessage': 'messages:write',
  'messages.setDraft': 'messages:write',
  'messages.togglePin': 'messages:write',
  'messages.toggleMute': 'messages:write',
  'messages.clear': 'messages:write',
  'messages.deleteConversation': 'messages:write',
  // A18 通讯录数据面：overview=contacts:read；写操作（申请应答/标签分组/
  // 拉黑/资料）=contacts:write（高危确认）
  'contacts.overview': 'contacts:read',
  'contacts.updateProfile': 'contacts:write',
  'contacts.setBlocked': 'contacts:write',
  'contacts.removeFriend': 'contacts:write',
  'contacts.sendRequest': 'contacts:write',
  'contacts.replyRequest': 'contacts:write',
  'contacts.askRequest': 'contacts:write',
  'contacts.resolveRequest': 'contacts:write',
  'contacts.tagCreate': 'contacts:write',
  'contacts.tagRename': 'contacts:write',
  'contacts.tagDelete': 'contacts:write',
  'contacts.groupCreate': 'contacts:write',
  'contacts.groupRename': 'contacts:write',
  'contacts.groupDelete': 'contacts:write',
  'contacts.groupMove': 'contacts:write',
  'contacts.setGroup': 'contacts:write',
  'contacts.orgGroupCreate': 'contacts:write',
  'contacts.orgGroupRename': 'contacts:write',
  'contacts.orgGroupDelete': 'contacts:write',
  'contacts.orgGroupMove': 'contacts:write',
  // 内容面 blob（public-topics §七 sdk.content）：读/拉取/列表归 storage:read，
  // 保存/根标记/GC 归 storage:write（与 data.readBlob/saveBlob 同权限口径）
  'content.readBlob': 'storage:read',
  'content.fetchBlob': 'storage:read',
  'content.listBlobs': 'storage:read',
  'content.saveBlob': 'storage:write',
  'content.pinRoot': 'storage:write',
  'content.unpinRoot': 'storage:write',
  'content.gcSweep': 'storage:write',
  // community-affairs §7.2：事务读/写（写含关注/取关/提交操作）、凭证只读、
  // 策略读/写。读位 affairs:read / credentials:read / policy:read，写位
  // affairs:write / policy:write；与内核 market/permissions.rs 逐字对齐。
  'affairs.listFollowed': 'affairs:read',
  'affairs.readLog': 'affairs:read',
  'affairs.readRules': 'affairs:read',
  'affairs.readResolution': 'affairs:read',
  'affairs.ladderStatus': 'affairs:read',
  'affairs.publicProfile': 'affairs:read',
  'affairs.snapshotPayload': 'affairs:read',
  'affairs.readExec': 'affairs:read',
  'affairs.orgEffects': 'affairs:read',
  'affairs.follow': 'affairs:write',
  'affairs.unfollow': 'affairs:write',
  'affairs.submitOp': 'affairs:write',
  // 回执编排写 org:effectrcpt: 并逐条存证（幂等、LWW）——与提交操作同写位
  'affairs.applyOrgEffects': 'affairs:write',
  'credentials.listHeld': 'credentials:read',
  'credentials.presentHolderProof': 'credentials:read',
  'credentials.queryVerifiers': 'credentials:read',
  'credentials.verify': 'credentials:read',
  'credentials.queryRevocations': 'credentials:read',
  'policy.read': 'policy:read',
  'policy.submitDraft': 'policy:write',
  // 发布合入（org:policydoc: 同步键域，管理员授权面）与草稿提交同写位
  'policy.publish': 'policy:write',
  // A34 市场模块（sdk.market）：市场命令等语义移植。读位 market:read
  // （目录/更新探测/仓库解析/侧载预览/广播索引查询），写位 market:write
  // （安装/更新/启停/卸载/侧载导入/发布声明）；与内核 market/permissions.rs
  // 逐字对齐
  'market.list': 'market:read',
  'market.checkUpdates': 'market:read',
  'market.resolveRepo': 'market:read',
  'market.inspectLocal': 'market:read',
  'market.announceList': 'market:read',
  'market.announceGet': 'market:read',
  'market.upgrade': 'market:write',
  'market.setEnabled': 'market:write',
  'market.uninstall': 'market:write',
  'market.installFromRepo': 'market:write',
  'market.importLocal': 'market:write',
  'market.announcePublish': 'market:write',
  // .spkg 文件选择对话框（壳层代开，用户主动行为，只回路径）归读位
  'market.pickSpkg': 'market:read',
  // A42 组织管理模块（sdk.org）：org-* 命令等语义移植。读位 org:read
  // （基础权限——组织列表/同步概览/网关活跃集/邀请记录/地址解析/节点名片/
  // 数据治理预览/导出），写位 org:write（高级——创建/退出/名册/邀请/公开/
  // 信息更新/导入名片/数据治理执行）；空间门控见 ORG_ID_SCOPED_CALLS
  'org.listMine': 'org:read',
  'org.getSyncOverview': 'org:read',
  'org.getGatewayActiveSet': 'org:read',
  'org.inviteRecords': 'org:read',
  'org.resolveAddress': 'org:read',
  'org.searchKnown': 'org:read',
  'org.makeNodeCard': 'org:read',
  'org.purgePreview': 'org:read',
  'org.exportData': 'org:read',
  'org.create': 'org:write',
  'org.leave': 'org:write',
  'org.addMember': 'org:write',
  'org.removeMember': 'org:write',
  'org.createInvite': 'org:write',
  'org.acceptInvite': 'org:write',
  'org.setPublic': 'org:write',
  'org.updateInfo': 'org:write',
  'org.updateMyIdentity': 'org:write',
  'org.sendInvite': 'org:write',
  'org.respondInvite': 'org:write',
  'org.importNodeCard': 'org:write',
  'org.purgeExecute': 'org:write'
  // 注：affairs.create 是桥 client 侧组合（identity.sign + affairs.follow，
  // 逐调用各自由本表强制）；affairs.onChange 走事件订阅通道（subscribe
  // 不经 call 表，与 data.onChange 同口径，事件载荷仅 affairId+变更类别
  // +opHash/status（submitted）或 accepted/drained 计数（replicated），
  // 均为哈希/计数，无事务内容），其 affairs:read 门控在 PluginIframeHost
  // 的 AffairChanged 转发处强制
};

/**
 * 联系人归属校验：插件只能操作自己注册的联系人。
 * contactId 约定格式 `bot:{pluginId}:{botId}`，pluginId 段必须与桥绑定身份一致，
 * 防止插件注册/注销/读写他人（其他插件或真人）的联系人。校验失败抛错（同步抛，
 * 在 dispatcher 的 try/catch 内被转为拒绝）。
 */
function assertOwnsContact(contactId: string, pluginId: string): void {
  const ownerPluginId = contactId.startsWith('bot:') ? contactId.split(':')[1] : undefined;
  if (ownerPluginId !== pluginId) {
    throw new Error(`Access denied: contact ${contactId} is not owned by plugin ${pluginId}`);
  }
}

/**
 * 社交投递 topic 前缀校验（架构 §8「topic 前缀即插件归属」，出站侧）：
 * 出站 deliver 的 topic 前缀必须 == 调用方插件 id，防止插件向他人 topic 投递
 * 或冒充其它插件归属。校验失败抛错（同步抛，dispatcher try/catch 内转拒绝）。
 */
function assertTopicOwned(topic: string, pluginId: string): void {
  const prefix = topic.split(':')[0] ?? topic;
  if (prefix !== pluginId) {
    throw new Error(
      `InvalidTopic: topic prefix "${prefix}" does not match plugin "${pluginId}"`
    );
  }
}

/** 桥侧消息 id 生成（壳层同口径 `m{ts}-{seq}`，见 stores/messages.ts sendText） */
let bridgeMessageSeq = 0;
function nextBridgeMessageId(): string {
  return `m${Date.now()}-${++bridgeMessageSeq}`;
}

// ------------------------------------------------------------------
// navigation 域参数白名单（sdk.navigation：插件 → 壳层纯 UI 导航意图，
// 免权限；校验失败即拒，不产出任何跳转）
// ------------------------------------------------------------------

/** openChat 会话名长度上限（防止插件注入超长标题撑破壳层 UI） */
const NAV_CHAT_NAME_MAX = 100;
/** openPlugin 的 cardData 序列化上限（视图引导只承载小型定位参数，如 affairId） */
const NAV_CARD_DATA_MAX_BYTES = 16 * 1024;

function assertNavChatInput(raw: unknown): { rootId: string; name?: string; conversationId?: string } {
  const input = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  const rootId = input.rootId;
  if (typeof rootId !== 'string' || !/^[0-9a-f]{64}$/.test(rootId)) {
    throw new Error('InvalidArgs: navigation.openChat requires rootId as 64-char lowercase hex');
  }
  const name = input.name;
  if (name !== undefined && (typeof name !== 'string' || name.length > NAV_CHAT_NAME_MAX)) {
    throw new Error(`InvalidArgs: navigation.openChat name must be a string of at most ${NAV_CHAT_NAME_MAX} chars`);
  }
  const conversationId = input.conversationId;
  if (conversationId !== undefined && (typeof conversationId !== 'string' || conversationId.length > 128)) {
    throw new Error('InvalidArgs: navigation.openChat conversationId must be a string of at most 128 chars');
  }
  return {
    rootId,
    ...(typeof name === 'string' ? { name } : {}),
    ...(typeof conversationId === 'string' ? { conversationId } : {})
  };
}

async function assertNavPluginInput(
  raw: unknown
): Promise<{ pluginId: string; viewId?: string; cardData?: unknown }> {
  const input = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  const pluginId = input.pluginId;
  if (typeof pluginId !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9-]{0,127}$/.test(pluginId)) {
    throw new Error('InvalidArgs: navigation.openPlugin pluginId must be a plugin id string');
  }
  // 目标插件不得由调用方任意伪造：只接受市场注册表内的插件 id（安装/启用/
  // 当前空间适配由深链既有链路处理：未装就地安装、不支持当前空间被 openPluginTab 拦）
  const items = await window.electronAPI.pluginMarket.list();
  if (!items.some((item) => item.id === pluginId)) {
    throw new Error(`Access denied: plugin ${pluginId} is not registered in the marketplace`);
  }
  const viewId = input.viewId;
  if (viewId !== undefined && (typeof viewId !== 'string' || viewId.length > 64)) {
    throw new Error('InvalidArgs: navigation.openPlugin viewId must be a string of at most 64 chars');
  }
  let cardData = input.cardData;
  if (cardData !== undefined) {
    let serialized: string;
    try {
      serialized = JSON.stringify(cardData) ?? '';
    } catch {
      throw new Error('InvalidArgs: navigation.openPlugin cardData must be JSON-serializable');
    }
    if (serialized.length > NAV_CARD_DATA_MAX_BYTES) {
      throw new Error(`InvalidArgs: navigation.openPlugin cardData exceeds ${NAV_CARD_DATA_MAX_BYTES} bytes`);
    }
    cardData = JSON.parse(serialized) as unknown;
  }
  return {
    pluginId,
    ...(typeof viewId === 'string' ? { viewId } : {}),
    ...(cardData !== undefined ? { cardData } : {})
  };
}

// ------------------------------------------------------------------
// sys.saveFile 入参白名单（壳层代存：插件沙箱 iframe 无对话框/文件写能力，
// 壳层代开保存对话框并代写用户所选路径——spark-files 下载通路，A42 修复）
// ------------------------------------------------------------------

/** 建议文件名长度上限（对话框 defaultPath） */
const SAVEFILE_NAME_MAX = 255;
/** 代存数据上限（base64 字符串长度；≈192MiB 解码后，防插件经桥灌爆主进程内存） */
const SAVEFILE_BASE64_MAX = 256 * 1024 * 1024;

function assertSaveFileInput(raw: unknown): { name: string; dataBase64: string } {
  const input = (typeof raw === 'object' && raw !== null ? raw : {}) as Record<string, unknown>;
  const name = input.name;
  if (
    typeof name !== 'string' ||
    name.length === 0 ||
    name.length > SAVEFILE_NAME_MAX ||
    // 仅文件名，不得携带路径分隔/控制字符，纯点名（"." / ".."）同拒
    // （它只是对话框建议名，不表达任何目录语义）
    /[/\\\u0000-\u001f]/.test(name) ||
    /^\.+$/.test(name)
  ) {
    throw new Error('InvalidArgs: sys.saveFile name must be a plain file name (no path separators/control chars, ≤255 chars)');
  }
  const dataBase64 = input.dataBase64;
  if (typeof dataBase64 !== 'string' || dataBase64.length > SAVEFILE_BASE64_MAX) {
    throw new Error(`InvalidArgs: sys.saveFile dataBase64 must be a base64 string of at most ${SAVEFILE_BASE64_MAX} chars`);
  }
  return { name, dataBase64 };
}

/** view type 裁剪表：null = 全量（仅 grantedPermissions 过滤）；未列出的 view type 整域拒绝 */
const VIEW_ALLOWED_CALLS: Record<PluginViewType, ReadonlySet<string> | null> = {
  app: null,
  // background 视图线形已合法化（A56）：无 UI 面，实例跑在内核 QuickJS 后台
  // 运行时（capability 由内核分发层强制），不经 iframe 桥——正常路径不会有
  // 桥绑定携带该 viewType；表项置空集 deny-by-default（纵深防御：异常绑定
  // 出现时 null 等价全量能力面，空集则整域拒绝）
  background: new Set(),
  // 消息卡片：docs/data 只读 + 验签/存证读取（无网络、无签名，设计文档「UI 集成点」）；
  // 不含 messages.*——卡片视图无应用会话写权限，卡片回调只经 action 上行（triggerCardAction）。
  // affairs.readLog/readResolution：议题卡片正文经 sdk.affairs 读本机副本的正当通道
  // （只读、不触网，affairs:read 授权仍由 CALL_PERMISSIONS 强制；写面一律不放行）
  'message-card': new Set(['docs.get', 'docs.query', 'data.get', 'data.query', 'data.readBlob', 'identity.verify', 'evidence.headHash', 'evidence.verify', 'affairs.readLog', 'affairs.readResolution'])
};

/**
 * org 域调用：需组织空间上下文，personal 空间下一律拒绝（无 org 实参可校验）。
 * A42 评审决议（口径统一）：org.listMine 与老面 runtime.listMineOrganizations
 * 同口径——personal 空间无组织可管，不向插件枚举本机组织名册（成员 rootId/
 * 角色/昵称）；插件 personal 视图据此不调 listMine，如实提示用户切换空间。
 */
const ORG_SPACE_CALLS = new Set([
  'runtime.syncOrganizationData',
  'runtime.listMineOrganizations',
  'org.listMine'
]);

/**
 * sdk.org 的 orgId 作用域调用（A42）：org 空间下 orgId 实参必须等于当前空间
 * id（防插件操作当前空间之外的组织数据），personal 空间一律拒绝。
 * org.sendInvite 的 orgId 在 input 对象内（args[0].orgId），其余在 args[0]。
 * 空间无关面（create/acceptInvite/resolveAddress/searchKnown/
 * respondInvite/importNodeCard/exportData、无 orgId 实参的 makeNodeCard）
 * 两空间放行——创建/加入/发现是身份级动作，不绑空间。
 * listMine 不在放行面：personal 空间拒（见 ORG_SPACE_CALLS 决议注释）。
 */
const ORG_ID_SCOPED_CALLS = new Set([
  'org.leave',
  'org.addMember',
  'org.removeMember',
  'org.createInvite',
  'org.getGatewayActiveSet',
  'org.getSyncOverview',
  'org.setPublic',
  'org.updateInfo',
  'org.updateMyIdentity',
  'org.sendInvite',
  'org.inviteRecords',
  'org.purgePreview',
  'org.purgeExecute'
]);

/** 提取 org 作用域调用的 orgId 实参（sendInvite 在 input 内，其余为首参） */
function orgIdArgOf(callKey: string, args: unknown[]): unknown {
  if (callKey === 'org.sendInvite') {
    return (args[0] as { orgId?: unknown } | undefined)?.orgId;
  }
  return args[0];
}

export type PluginBridgeIdentity = {
  pluginId: string;
  viewId: string;
  /** 插件域身份（plugin: 前缀，由 pluginId 推出） */
  domain: string;
  space: PluginSpaceContext;
  /** 插件显示名（使用时询问文案）；缺省用 pluginId */
  pluginName?: string;
  /** manifest supportedSpaces（缺省 = 不做 space 类型校验） */
  supportedSpaces?: Array<'personal' | 'org'>;
  /** 视图类型（默认 app） */
  viewType?: PluginViewType;
  /** 插件请求关闭自身视图（sdk.close()）时触发 */
  onClose?: () => void;
};

/** 使用时询问的会话级决定记忆：key = `${pluginId}|${domain}`（pluginName 仅用于弹窗文案） */
const useTimeConsent = new Set<string>();
/** 确认进行中的 in-flight Promise：并发首调复用同一确认，避免弹多个框 */
const pendingConsent = new Map<string, Promise<void>>();

/** identity:sign 首次使用确认；用户拒绝抛 Access denied */
async function confirmIdentitySign(pluginId: string, pluginName: string, domain: string): Promise<void> {
  const consentKey = `${pluginId}|${domain}`;
  if (useTimeConsent.has(consentKey)) {
    return;
  }
  const inflight = pendingConsent.get(consentKey);
  if (inflight) {
    return inflight;
  }
  const prompt = (async () => {
    try {
      await ElMessageBox.confirm(
        `应用「${pluginName}」（${domain}）请求使用插件域身份签名。签名以该应用域身份出具，可用于数据确权与存证，是否允许？（本次会话内记住选择）`,
        '签名确认',
        { confirmButtonText: '允许', cancelButtonText: '拒绝', type: 'warning' }
      );
    } catch {
      throw new Error('Access denied: identity:sign rejected by user');
    }
    useTimeConsent.add(consentKey);
  })();
  pendingConsent.set(consentKey, prompt);
  try {
    await prompt;
  } finally {
    pendingConsent.delete(consentKey);
  }
}

/**
 * 构造桥 handler：先读 grantedPermissions 与后端实例，返回逐调用过滤的分发器。
 */
export async function createPluginBridgeDispatcher(identity: PluginBridgeIdentity): Promise<BridgeHostHandler> {
  const viewType = identity.viewType ?? 'app';
  const pluginName = identity.pluginName ?? identity.pluginId;

  // 三重过滤之一：grantedPermissions（内核持久化的安装授权；读取失败按空清单）
  let granted = new Set<string>();
  try {
    const items = await window.electronAPI.pluginMarket.list();
    granted = new Set(items.find((item) => item.id === identity.pluginId)?.grantedPermissions ?? []);
  } catch {
    // 最小授权：仅免权限基础调用放行
  }

  // O3 org 上下文：data.* 读写经 createPluginBackend 注入 orgId（org space
  // 取 identity.space.id；personal space 为 undefined）——插件 SDK 签名不变，
  // orgId 由内核按插件实例所属空间解析，桥按绑定身份下发。
  const boundOrgId = identity.space.type === 'org' ? identity.space.id : undefined;
  // messages/contacts 数据面（A18）：pluginId/space 由桥按绑定身份注入
  // （插件自报一律忽略），先于 backend 构造供其注入
  const boundSpaceKey = identity.space.type === 'org' ? `org:${identity.space.id}` : 'personal';
  const backend = createPluginBackend(identity.domain, boundOrgId, identity.onClose, boundSpaceKey);

  // messages 域：pluginId/space 由桥按绑定身份注入（插件自报一律忽略）。
  // pluginId 剥离域前缀（'plugin:spark-example' → 'spark-example'，§20.1 存储键口径；
  // 与 identity.pluginId 同源，domain 推导失败时回退绑定 pluginId）
  const boundPluginId = identity.domain.startsWith('plugin:')
    ? identity.domain.slice('plugin:'.length)
    : identity.pluginId;

  const modules: Record<string, Record<string, (...args: any[]) => Promise<unknown>>> = {
    // 应用级控制（免权限）：插件请求关闭自身视图
    app: {
      close: async () => {
        identity.onClose?.();
      }
    },
    // 导航意图（免权限基础调用，纯 UI 跳转无数据面暴露）：沙箱 iframe 内
    // CustomEvent 不出浏览上下文，插件「发消息/打开其他插件」意图必须经此
    // 上行。参数白名单校验后路由到壳层既有链路（spark:open-chat 事件 /
    // 统一深链事件保留为壳内机制，壳层监听方不变）
    navigation: {
      openChat: async (input: unknown) => {
        const detail = assertNavChatInput(input);
        window.dispatchEvent(new CustomEvent('spark:open-chat', { detail }));
      },
      openPlugin: async (input: unknown) => {
        const detail = await assertNavPluginInput(input);
        window.dispatchEvent(new CustomEvent(OPEN_PLUGIN_DEEPLINK_EVENT, { detail }));
      }
    },
    docs: {
      get: backend.docs.get,
      defineCollection: backend.docs.defineCollection,
      put: backend.docs.put,
      delete: backend.docs.delete,
      query: backend.docs.query
    },
    data: {
      declareCollection: backend.data.declareCollection,
      save: backend.data.save,
      delete: backend.data.delete,
      get: backend.data.get,
      query: backend.data.query,
      dropVersion: backend.data.dropVersion,
      saveBlob: backend.data.saveBlob,
      readBlob: backend.data.readBlob
    },
    identity: {
      sign: backend.identity.sign,
      verify: backend.identity.verify
    },
    evidence: {
      headHash: backend.evidence.headHash,
      verify: backend.evidence.verify
    },
    p2p: {
      start: backend.p2p.start,
      stop: backend.p2p.stop,
      broadcast: backend.p2p.broadcast
    },
    runtime: {
      currentRoot: backend.runtime.currentRoot,
      syncOrganizationData: backend.runtime.syncOrganizationData,
      listMineOrganizations: backend.runtime.listMineOrganizations
    },
    // 统一消息模块：IM 数据面（A18，等语义移植现有 Tauri 消息命令，
    // space 由桥绑定注入）+ 服务号（应用会话 §20）+ 插件联系人
    messages: {
      // A18 IM 数据面（messages:read/write 经 CALL_PERMISSIONS 强制）
      conversations: () => window.electronAPI.messages.listConversations(boundSpaceKey),
      list: (convId: string) => window.electronAPI.messages.listMessages(boundSpaceKey, convId),
      send: (convId: string, text: string, quote?: { messageId: string; senderName: string; preview: string } | null, messageId?: string | null) =>
        // messageId 与壳层同语义：插件透传乐观入列的自生成 id（等语义移植
        // message-send-text），缺省由桥按壳层同口径生成（`m{ts}-{seq}`）
        window.electronAPI.messages.sendText(
          boundSpaceKey,
          convId,
          typeof messageId === 'string' && messageId ? messageId : nextBridgeMessageId(),
          text,
          quote ?? undefined
        ),
      recall: (convId: string, messageId: string) =>
        window.electronAPI.messages.recall(boundSpaceKey, convId, messageId),
      markConversationRead: (convId: string) =>
        window.electronAPI.messages.markRead(boundSpaceKey, convId),
      // A19 写面补全（等语义移植现有 Tauri 命令，space 桥绑定注入）
      ensureDirect: (peerId: string, title: string) =>
        window.electronAPI.messages.ensureDirect(boundSpaceKey, peerId, title),
      resend: (convId: string, messageId: string) =>
        window.electronAPI.messages.resend(boundSpaceKey, convId, messageId),
      deleteMessage: (convId: string, messageId: string) =>
        window.electronAPI.messages.deleteMessage(boundSpaceKey, convId, messageId),
      setDraft: (convId: string, draft: string) =>
        window.electronAPI.messages.setDraft(boundSpaceKey, convId, draft),
      togglePin: (convId: string) =>
        window.electronAPI.messages.togglePin(boundSpaceKey, convId),
      toggleMute: (convId: string) =>
        window.electronAPI.messages.toggleMute(boundSpaceKey, convId),
      clear: (convId: string) =>
        window.electronAPI.messages.clear(boundSpaceKey, convId),
      deleteConversation: (convId: string) =>
        window.electronAPI.messages.deleteConversation(boundSpaceKey, convId),
      sendAppMessage: (payload: Record<string, unknown>, card?: AppMessageCardDto) =>
        sendAppMessage(boundSpaceKey, boundPluginId, payload, card),
      listAppMessages: () => listAppMessages(boundSpaceKey, boundPluginId),
      markRead: () => markAppMessagesRead(boundSpaceKey, boundPluginId),
      registerAsContact: (contactId: string, displayName: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        return invoke('contact_ensure_bot', {
          botRootId: contactId,
          displayName
        }).then(async (result) => {
          // ensure_bot 是内核本地写入，不产生 P2P 事件——通讯录缓存
          // 首次水合后不会自动感知，主动刷新让新 bot 立即出现在列表
          await refreshContacts(boundSpaceKey);
          // 统一打上"以插件显示名命名"的标签，标识联系人来源于哪个插件
          ensurePluginContactTag(boundSpaceKey, identity.pluginName || identity.pluginId, contactId);
          return result;
        });
      },
      unregisterAsContact: (contactId: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        // 插件删除 bot 时注销联系人：内核删好友记录 + 刷新缓存让列表立即移除
        return invoke('contact_remove_friend', { rootId: contactId, block: false })
          .then(async (result) => {
            await refreshContacts(boundSpaceKey);
            return result;
          });
      },
      sendResponse: (convId: string, contactId: string, displayName: string, messageId: string, text: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        return invoke('message_bot_reply', {
          spaceKey: boundSpaceKey,
          convId,
          botRootId: contactId,
          botName: displayName,
          messageId,
          text
        });
      }
      // waitForMessage（长轮询监听 bot 消息）已随后台运行时迁移下线：
      // bot 消息由内核直接推送到插件的 QuickJS 后台线程（spark.onMessage），
      // iframe 视图不再有消费 bot 消息的场景
    },
    // 通讯录只读门面（社交投递层 §9.4 contact:read；CALL_PERMISSIONS 强制；
    // 经 backend.contacts 走 electronAPI.contacts，与 docs/data 域一致；
    // backend.contacts 在 SDK 类型上为可选，但 createPluginBackend 恒注入，非空断言同 sys）
    contacts: {
      listFriends: () => backend.contacts!.listFriends(),
      listGroups: () => backend.contacts!.listGroups(),
      listTags: () => backend.contacts!.listTags(),
      // A18 数据面（等语义移植现有 Tauri 通讯录命令；space 经 backend 按
      // 绑定注入；contacts:read/write 经 CALL_PERMISSIONS 强制）
      overview: () => backend.contacts!.overview(),
      updateProfile: (rootId: string, patch: Parameters<NonNullable<PluginSDK['contacts']>['updateProfile']>[1]) =>
        backend.contacts!.updateProfile(rootId, patch),
      setBlocked: (rootId: string, blocked: boolean) => backend.contacts!.setBlocked(rootId, blocked),
      removeFriend: (rootId: string, block?: boolean) => backend.contacts!.removeFriend(rootId, block),
      sendRequest: (input: Parameters<NonNullable<PluginSDK['contacts']>['sendRequest']>[0]) =>
        backend.contacts!.sendRequest(input),
      replyRequest: (requestId: string, text: string) => backend.contacts!.replyRequest(requestId, text),
      askRequest: (requestId: string, text: string) => backend.contacts!.askRequest(requestId, text),
      resolveRequest: (requestId: string, accept: boolean, permission: 'open' | 'chatOnly') =>
        backend.contacts!.resolveRequest(requestId, accept, permission),
      tagCreate: (id: string, name: string) => backend.contacts!.tagCreate(id, name),
      tagRename: (tagId: string, name: string) => backend.contacts!.tagRename(tagId, name),
      tagDelete: (tagId: string) => backend.contacts!.tagDelete(tagId),
      groupCreate: (id: string, name: string) => backend.contacts!.groupCreate(id, name),
      groupRename: (groupId: string, name: string) => backend.contacts!.groupRename(groupId, name),
      groupDelete: (groupId: string) => backend.contacts!.groupDelete(groupId),
      groupMove: (groupId: string, toIndex: number) => backend.contacts!.groupMove(groupId, toIndex),
      setGroup: (rootId: string, groupId: string) => backend.contacts!.setGroup(rootId, groupId),
      orgGroupCreate: (parentId: string, id: string, name: string) =>
        backend.contacts!.orgGroupCreate(parentId, id, name),
      orgGroupRename: (id: string, name: string) => backend.contacts!.orgGroupRename(id, name),
      orgGroupDelete: (id: string) => backend.contacts!.orgGroupDelete(id),
      orgGroupMove: (id: string, toIndex: number, newParentId?: string) =>
        backend.contacts!.orgGroupMove(id, toIndex, newParentId)
    },
    // 社交定向投递（social-feed §9.1 sdk.feed）。deliver 经 CALL_PERMISSIONS
    // 强制 feed:deliver + 出站 topic 前缀校验（架构 §8）；pull/onReceive 接收侧
    // 免权限（onReceive 为事件订阅，不在此 call 表内，由 PluginIframeHost 经桥
    // FeedReceived 事件推送）。pluginId 由桥绑定身份注入（不信插件自报）。
    feed: {
      deliver: (input: {
        topic: string;
        payload: unknown;
        recipients: string[];
        replyTo?: string;
        feedId?: string;
      }) => {
        assertTopicOwned(input.topic, identity.pluginId);
        return backend.feed!.deliver(input);
      },
      pull: (input: { topic: string; cursor?: string; limit?: number }) => {
        // B2：pull 与 deliver 同构做 topic 前缀归属校验——防任一插件
        // `sdk.feed.pull({topic:"spark-moments:posts"})` 读他人收件箱。
        // 桥校验 + 壳层 feed_pull_inner 校验双保险（不信插件自报）。
        assertTopicOwned(input.topic, identity.pluginId);
        return backend.feed!.pull(input);
      }
    },
    // 内容面 blob（public-topics §七 sdk.content；CALL_PERMISSIONS 强制
    // storage:read/storage:write；CID 形状与哈希校验在内核兜底）
    content: {
      saveBlob: (dataBase64: string) => backend.content!.saveBlob(dataBase64),
      readBlob: (cid: string) => backend.content!.readBlob(cid),
      fetchBlob: (cid: string) => backend.content!.fetchBlob(cid),
      listBlobs: () => backend.content!.listBlobs(),
      pinRoot: (cid: string, root: string) => backend.content!.pinRoot(cid, root),
      unpinRoot: (cid: string, root: string) => backend.content!.unpinRoot(cid, root),
      gcSweep: () => backend.content!.gcSweep()
    },
    // community-affairs §7.2 sdk.affairs：内核 affair 门面薄壳（权限经
    // CALL_PERMISSIONS 强制 affairs:read/affairs:write；affairId 自报面由
    // 内核门面以创世复算/形态校验兜底）
    affairs: {
      follow: (genesis: Record<string, unknown>) => backend.affairs!.follow(genesis),
      unfollow: (affairId: string) => backend.affairs!.unfollow(affairId),
      listFollowed: () => backend.affairs!.listFollowed(),
      submitOp: (op: Record<string, unknown>) => backend.affairs!.submitOp(op),
      readLog: (affairId: string) => backend.affairs!.readLog(affairId),
      readRules: (affairId: string) => backend.affairs!.readRules(affairId),
      readResolution: (affairId: string) => backend.affairs!.readResolution(affairId),
      ladderStatus: (affairId: string) => backend.affairs!.ladderStatus(affairId),
      publicProfile: (identity: string) => backend.affairs!.publicProfile(identity),
      snapshotPayload: (affairId: string, asOf?: string) => backend.affairs!.snapshotPayload(affairId, asOf),
      readExec: (affairId: string) => backend.affairs!.readExec(affairId),
      orgEffects: (orgId: string, affairId: string) => backend.affairs!.orgEffects(orgId, affairId),
      applyOrgEffects: (orgId: string, affairId: string) => backend.affairs!.applyOrgEffects(orgId, affairId)
    },
    // community-affairs §7.2 sdk.credentials：backend 已按绑定域注入签名域
    // （presentHolderProof 的 pluginDomain 插件不可自报）
    credentials: {
      listHeld: () => backend.credentials!.listHeld(),
      presentHolderProof: (input: { credId: string; requestId: string; orgId: string; collection: string }) =>
        backend.credentials!.presentHolderProof(input),
      queryVerifiers: (orgId: string) => backend.credentials!.queryVerifiers(orgId),
      // 桥入参为插件自报的宽松 JSON；结构校验在内核验证链（§6 第 1–5 步），此处仅做类型过渡
      verify: (credential: Record<string, unknown>) =>
        backend.credentials!.verify(credential as Parameters<PluginCredentialsAPI['verify']>[0]),
      queryRevocations: (issuer: string) => backend.credentials!.queryRevocations(issuer)
    },
    // community-affairs §7.2 sdk.policy：本地策略草稿读写 + 发布合入
    policy: {
      read: (orgId: string) => backend.policy!.read(orgId),
      submitDraft: (doc: Record<string, unknown>) => backend.policy!.submitDraft(doc),
      publish: (orgId: string) => backend.policy!.publish(orgId)
    },
    // A34 市场模块（sdk.market）：backend.market 为唯一事实源（sdk-browser
    // createPluginBackend 直连 electronAPI.pluginMarket；命令侧 domain_guard
    // 要求系统域，壳层主窗口满足），此处仅按桥入参收口 null→undefined。
    // 权限经 CALL_PERMISSIONS 强制 market:read/market:write；市场操作是
    // 系统层本机动作，不按 space 设卡（与壳层应用管理同口径）。
    // onAnnounceChanged 不经后端：事件面由 PluginIframeHost 转发（A34）
    market: {
      list: () => backend.market!.list(),
      checkUpdates: (pluginId?: string | null) =>
        backend.market!.checkUpdates(typeof pluginId === 'string' && pluginId ? pluginId : undefined),
      upgrade: (pluginId: string) => backend.market!.upgrade(pluginId),
      setEnabled: (pluginId: string, enabled: boolean) => backend.market!.setEnabled(pluginId, enabled),
      uninstall: (pluginId: string) => backend.market!.uninstall(pluginId),
      resolveRepo: (id: string) => backend.market!.resolveRepo(id),
      installFromRepo: (id: string) => backend.market!.installFromRepo(id),
      inspectLocal: (path: string) => backend.market!.inspectLocal(path),
      importLocal: (path: string, expectedSha256: string, confirmOverwrite?: boolean) =>
        backend.market!.importLocal(path, expectedSha256, confirmOverwrite),
      announcePublish: (input: Parameters<PluginMarketAPI['announcePublish']>[0]) =>
        backend.market!.announcePublish(input),
      announceList: () => backend.market!.announceList(),
      announceGet: (id: string) => backend.market!.announceGet(id),
      // 插件沙箱 iframe 无系统对话框能力：.spkg 文件选择由壳层代开
      // （tauri-plugin-dialog，与旧侧载入口同链路），用户取消返回 null
      pickSpkg: () => backend.market!.pickSpkg()
    },
    // A42 组织管理模块（sdk.org）：org-* 命令等语义移植（backend.org 透传
    // electronAPI.organization / p2p / dataManagement）；权限经
    // CALL_PERMISSIONS 强制 org:read/org:write，orgId 作用域调用的空间
    // 门控在下方返回的 handler 内统一强制（ORG_ID_SCOPED_CALLS）
    org: {
      listMine: () => backend.org!.listMine(),
      create: (input: Parameters<NonNullable<PluginSDK['org']>['create']>[0]) => backend.org!.create(input),
      leave: (orgId: string) => backend.org!.leave(orgId),
      addMember: (orgId: string, input: { rootId: string; nodeInfo?: unknown }) =>
        backend.org!.addMember(orgId, input as Parameters<NonNullable<PluginSDK['org']>['addMember']>[1]),
      removeMember: (orgId: string, memberRootId: string) => backend.org!.removeMember(orgId, memberRootId),
      getGatewayActiveSet: (orgId: string) => backend.org!.getGatewayActiveSet(orgId),
      createInvite: (orgId: string) => backend.org!.createInvite(orgId),
      acceptInvite: (code: string) => backend.org!.acceptInvite(code),
      getSyncOverview: (orgId: string) => backend.org!.getSyncOverview(orgId),
      setPublic: (orgId: string, isPublic: boolean, displayName?: string) =>
        backend.org!.setPublic(orgId, isPublic, displayName),
      updateInfo: (orgId: string, patch: { name?: string; description?: string; avatar?: string }) =>
        backend.org!.updateInfo(orgId, patch),
      updateMyIdentity: (orgId: string, patch: Parameters<NonNullable<PluginSDK['org']>['updateMyIdentity']>[1]) =>
        backend.org!.updateMyIdentity(orgId, patch),
      resolveAddress: (orgAddress: string) => backend.org!.resolveAddress(orgAddress),
      searchKnown: (keyword: string) => backend.org!.searchKnown(keyword),
      sendInvite: (input: Parameters<NonNullable<PluginSDK['org']>['sendInvite']>[0]) =>
        backend.org!.sendInvite(input),
      respondInvite: (input: { inviteId: string; accept: boolean }) => backend.org!.respondInvite(input),
      inviteRecords: (orgId: string) => backend.org!.inviteRecords(orgId),
      makeNodeCard: (orgId?: string) => backend.org!.makeNodeCard(typeof orgId === 'string' && orgId ? orgId : undefined),
      importNodeCard: (card: string) => backend.org!.importNodeCard(card),
      purgePreview: (orgId: string, beforeTs: number) => backend.org!.purgePreview(orgId, beforeTs),
      purgeExecute: (orgId: string, beforeTs: number, confirmExported: boolean) =>
        backend.org!.purgeExecute(orgId, beforeTs, confirmExported),
      exportData: () => backend.org!.exportData()
    },
    // sys 代理（内核外呼）：仅代理不加工；插件享有完整权限，内核命令侧负责业务安全
    sys: {
      exec: (program: string, execArgs: string[], workdir?: string) => {
        console.log(`[bridge] sys.exec 收到 program=${program} args=${JSON.stringify(execArgs)} workdir=${workdir ?? '(继承)'}`);
        return backend.sys!.exec(program, Array.isArray(execArgs) ? execArgs : [], workdir) as Promise<unknown>;
      },
      fetch: (url: string, options?: Record<string, unknown>) =>
        backend.sys!.fetch(url, options) as Promise<unknown>,
      pickFolder: (title?: string) =>
        backend.sys!.pickFolder!(title) as Promise<unknown>,
      // 壳层代存（A42 修复：spark-files 下载通路）：沙箱 iframe 无 allow-downloads，
      // Blob 锚点下载各 WebView 口径不一——改为壳层代开保存对话框 + 代写用户所选
      // 路径（同 market.pickSpkg / org.exportData 先例）。入参白名单校验后转发
      saveFile: (input: unknown) => {
        const { name, dataBase64 } = assertSaveFileInput(input);
        return saveFileWithDialog(name, dataBase64);
      },
      fetchStream: async (url: string, options?: Record<string, unknown>) => {
        // 1. 发起流式请求，取得 streamId
        const { streamId } = await backend.sys!.fetchStream!(url, options) as { streamId: string };
        const eventName = `sys-stream:${streamId}`;

        // 2. 监听 Tauri 事件，逐块转发为桥 event
        const unlisten: UnlistenFn = await listen(eventName, (event) => {
          const chunk = event.payload as { text: string; done: boolean; status: number; headers: Record<string, string> };
          if (_eventPump) {
            _eventPump.pushEvent(eventName, chunk);
          }
          if (chunk.done) {
            unlisten();
          }
        });

        return { streamId };
      }
    },
  };

  return async (module, method, args) => {
    const callKey = `${module}.${method}`;
    const fn = modules[module]?.[method] as ((...callArgs: unknown[]) => Promise<unknown>) | undefined;
    if (!fn) {
      throw new Error(`Access denied: unknown SDK call ${callKey}`);
    }

    // 三重过滤之二：view type 裁剪
    const viewAllowed = VIEW_ALLOWED_CALLS[viewType];
    if (viewAllowed && !viewAllowed.has(callKey)) {
      throw new Error(`Access denied: ${callKey} is not available in ${viewType} view`);
    }

    // 三重过滤之一（续）：grantedPermissions
    const required = CALL_PERMISSIONS[callKey];
    if (required && !granted.has(required)) {
      throw new Error(`Access denied: permission "${required}" is not granted for plugin ${identity.pluginId}`);
    }

    // 三重过滤之三：当前 space
    if (identity.supportedSpaces && !identity.supportedSpaces.includes(identity.space.type)) {
      throw new Error(
        `Access denied: plugin ${identity.pluginId} does not support ${identity.space.type} space`
      );
    }
    // personal 空间无 org 上下文：org 域调用一律拒绝
    if (ORG_SPACE_CALLS.has(callKey) && identity.space.type === 'personal') {
      throw new Error(`Access denied: ${callKey} requires org space`);
    }
    if (
      callKey === 'runtime.syncOrganizationData' &&
      identity.space.type === 'org' &&
      args[0] !== identity.space.id
    ) {
      throw new Error(`Access denied: org ${String(args[0])} is outside current space ${identity.space.id}`);
    }
    // sdk.org 的 orgId 作用域调用（A42）：personal 空间整组拒绝；org 空间下
    // orgId 实参须等于当前空间 id。makeNodeCard 仅在带 orgId 实参时按作用域校验
    const orgScoped =
      ORG_ID_SCOPED_CALLS.has(callKey) ||
      (callKey === 'org.makeNodeCard' && typeof args[0] === 'string' && args[0].length > 0);
    if (orgScoped) {
      if (identity.space.type !== 'org') {
        throw new Error(`Access denied: ${callKey} requires org space`);
      }
      const orgIdArg = orgIdArgOf(callKey, args);
      if (orgIdArg !== identity.space.id) {
        throw new Error(`Access denied: org ${String(orgIdArg)} is outside current space ${identity.space.id}`);
      }
    }

    // 使用时询问：identity:sign 首次确认（会话级记忆，并发首调复用同一确认）
    if (callKey === 'identity.sign') {
      await confirmIdentitySign(identity.pluginId, pluginName, identity.domain);
    }

    return fn(...args);
  };
}
