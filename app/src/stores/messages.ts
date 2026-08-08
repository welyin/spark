/**
 * 消息 store（设计 §2/§3/§5/§9）：内核真实数据 + 内存响应式缓存的接入层。
 *
 * 数据流：内核 `window.electronAPI.messages.*`（Tauri 命令）为唯一真源，
 * 本文件维护按空间 key 隔离的响应式缓存（'personal' 个人空间 / 'org:<orgId>'
 * 组织空间）。所有读写仍收敛在本文件的 store 方法上（发送/置顶/免打扰/清空/
 * 删除/撤回等），组件零改动：
 * - 读：同步返回缓存；Tauri 环境下首次访问时异步水合（listConversations /
 *   listMessages），完成后响应式自动刷新。
 * - 写：本地缓存同步更新（保持组件的同步语义），随后 fire-and-forget 调内核
 *   持久化（静默 catch，失败不回滚本地态）。
 * - 远端推送：经 listenP2pEvents 订阅 ChatReceived（新消息）/ ChatStatus
 *   （已读/撤回/状态流转）事件就地合并进缓存。
 * 非 Tauri 环境（vitest / 纯前端预览）不发任何调用，退化为纯内存 store。
 *
 * 链接预览（§6）：发送方壳层（src-tauri）抓取 OG/Twitter Card 元数据随消息
 * 携带投递，接收方只展示不访问 URL。发送时先上 `buildLinkPreview` 的诚实占位
 * （域名白名单站点名 + 空描述），抓取结果随 sendText 的 dto.link 回来后替换；
 * 非 Tauri/demo 环境就停留在诚实占位。
 */
import { computed, reactive, watch } from 'vue';
import { isTauri, listenP2pEvents, type AppMessageCardDto, type AppMessageDto, type ElectronAPI } from '../api';
import { isAppConversationBlocked, SYSTEM_APP_PLUGIN_ID } from './app-conversations';

/** 空间 key：个人空间为 'personal'，组织空间为 'org:<orgId>' */
export type SpaceKey = string;

export type MessageType = 'text' | 'image' | 'file' | 'link' | 'voice' | 'system';
/** 消息状态（设计 §3.3）：发送中/已发送/已送达/已读/发送失败 */
export type MessageStatus = 'sending' | 'sent' | 'delivered' | 'read' | 'failed';

/** 链接预览卡片（设计 §6），元数据由发送方本地抓取随消息携带 */
export interface LinkPreview {
  url: string;
  title: string;
  description: string;
  /** 来源 APP 名（域名白名单映射），未知域名时等于 domain */
  siteName: string;
  domain: string;
}

/** 引用回复携带的原消息片段（设计 §9.3） */
export interface QuoteRef {
  messageId: string;
  senderName: string;
  preview: string;
}

export interface ChatMessage {
  id: string;
  senderId: string;
  senderName: string;
  type: MessageType;
  /** 文本内容；文件消息为文件名；系统消息为提示文案 */
  content: string;
  /** 文件大小（字节），仅文件消息 */
  fileSize?: number;
  /** 语音时长（秒），仅语音消息 */
  duration?: number;
  link?: LinkPreview;
  quote?: QuoteRef;
  createdAt: number;
  /** 仅自己发送的消息有状态 */
  status?: MessageStatus;
  recalled: boolean;
}

export interface Conversation {
  id: string;
  /** direct=1:1 单聊；system=系统通知/组织公告（设计 §8.3）；
   *  app=应用会话（服务号模型 §20，id 约定 `app:{pluginId}`，peerId 占位填 pluginId） */
  kind: 'direct' | 'system' | 'app';
  title: string;
  peerId: string;
  unreadCount: number;
  /** 置顶时间戳，0 表示未置顶（设计 §2.3） */
  pinnedAt: number;
  muted: boolean;
  /** 对方在线状态，由内核 P2P 连接状态推导（水合/ensureDirect 返回值携带） */
  online: boolean;
  /** 草稿文本，列表显示「[草稿]」前缀 */
  draft: string;
  /** 最后一条消息时间 */
  updatedAt: number;
}

interface SpaceData {
  conversations: Conversation[];
  messages: Record<string, ChatMessage[]>;
  /** 应用消息（§20）：键为应用会话 id（`app:{pluginId}`），与人际消息分开存储 */
  appMessages: Record<string, AppMessageDto[]>;
}

const MIN = 60_000;

/** 本地生成消息 id 的自增序号（id 形如 `m${Date.now()}-${seq}`，前端生成后随发送传入内核落库） */
let seq = 0;

function makeConversation(
  partial: Pick<Conversation, 'kind' | 'title' | 'peerId' | 'updatedAt'> & Partial<Conversation>
): Conversation {
  return { id: `dm:${partial.peerId}`, unreadCount: 0, pinnedAt: 0, muted: false, draft: '', online: false, ...partial };
}

/** 域名 → 来源 APP 白名单（设计 §6.3） */
const KNOWN_SITES: Record<string, string> = {
  'zhihu.com': '知乎',
  'weibo.com': '微博',
  'github.com': 'GitHub'
};

/**
 * 发送前的诚实占位卡片（§6）：真实元数据由发送方壳层（src-tauri）抓取，
 * 随 message-send-text 的 dto.link 回来后替换本地占位；Tauri 抓取失败或非
 * Tauri/demo 环境就停留在这个占位——只展示能确定的事实（域名白名单站点名），
 * 不编造标题/描述。
 */
export function buildLinkPreview(url: string): LinkPreview {
  let domain = url;
  try {
    domain = new URL(url).hostname.replace(/^www\./, '');
  } catch {
    // 非法 URL 时原样展示
  }
  const siteName = KNOWN_SITES[domain] ?? domain;
  return { url, domain, siteName, title: siteName, description: '' };
}

// ---------- 响应式缓存 ----------

const spaces = reactive<Record<SpaceKey, SpaceData>>({});
/** 当前打开的会话（按空间），用于决定新消息是否计入未读 */
const activeConversation = reactive<Record<SpaceKey, string>>({});
/** 已触发过消息水合的会话（`${spaceKey}\n${convId}`），避免重复拉取 */
const hydratedMessages = new Set<string>();
/** P2P 事件订阅是否已初始化（模块级懒初始化，仅 Tauri 环境） */
let eventsSubscribed = false;

type MessagesApi = ElectronAPI['messages'];

/** 内核消息接口：仅 Tauri 环境且宿主 API 已安装时可用；否则返回 undefined（纯内存模式） */
function messagesApi(): MessagesApi | undefined {
  if (!isTauri()) return undefined;
  return (window as unknown as { electronAPI?: ElectronAPI }).electronAPI?.messages;
}

type SystemApi = ElectronAPI['system'];

/** 系统桥接接口（徽标等）：守卫同 messagesApi，非 Tauri 环境返回 undefined */
function systemApi(): SystemApi | undefined {
  if (!isTauri()) return undefined;
  return (window as unknown as { electronAPI?: ElectronAPI }).electronAPI?.system;
}

/** 空间 key 约定：重新导出自 space-key 工具模块 */
export { spaceKeyOf } from '../mock/space-key';

function ensureSpace(key: SpaceKey): SpaceData {
  if (!spaces[key]) {
    spaces[key] = { conversations: [], messages: {}, appMessages: {} };
    subscribeP2pEvents();
    hydrateConversations(key);
  }
  return spaces[key];
}

/**
 * 登录态切换时清空消息缓存（RootGate 登出/切换账号时调用）。
 * spaces/hydratedMessages 为窗口会话级单例，登出不刷新页面时旧账号的
 * 会话与消息会带进新登录会话——清空后重新从内核水合。
 */
export function resetMessagesCache(): void {
  for (const key of Object.keys(spaces)) {
    delete spaces[key];
  }
  for (const key of Object.keys(activeConversation)) {
    delete activeConversation[key];
  }
  hydratedMessages.clear();
}

/** 首次进入空间时拉取会话列表水合缓存；按 id merge，保留水合期间本地新建的会话 */
function hydrateConversations(key: SpaceKey): void {
  const api = messagesApi();
  if (!api) return;
  void api
    .listConversations(key)
    .then((dtos) => {
      const space = spaces[key];
      if (!space) return;
      const localOnly = space.conversations.filter((c) => !dtos.some((d) => d.id === c.id));
      space.conversations = [...dtos.map((d) => ({ ...d })), ...localOnly];
    })
    .catch(() => {});
}

/** 应用会话 id 前缀（§20.1：会话 id = `app:{pluginId}`） */
const APP_CONV_PREFIX = 'app:';

/** 应用会话 id → pluginId；非应用会话返回 null */
export function appConversationPluginId(convId: string): string | null {
  return convId.startsWith(APP_CONV_PREFIX) ? convId.slice(APP_CONV_PREFIX.length) : null;
}

/** 首次读取某会话消息时拉取历史水合缓存；按 id merge，保留水合期间本地新增的消息 */
function hydrateMessages(key: SpaceKey, convId: string): void {
  const loadedKey = `${key}\n${convId}`;
  if (hydratedMessages.has(loadedKey)) return;
  hydratedMessages.add(loadedKey);
  const api = messagesApi();
  if (!api) return;
  const pluginId = appConversationPluginId(convId);
  if (pluginId !== null) {
    // 应用会话：消息在 `msg:app:` 键空间，走 appList 水合（§20.6）
    void api
      .appList(key, pluginId)
      .then((dtos) => mergeAppMessages(key, convId, dtos))
      .catch(() => {});
    return;
  }
  void api
    .listMessages(key, convId)
    .then((dtos) => {
      const space = spaces[key];
      if (!space) return;
      const localOnly = (space.messages[convId] ?? []).filter((m) => !dtos.some((d) => d.id === m.id));
      space.messages[convId] = [...dtos.map((d) => ({ ...d })), ...localOnly];
    })
    .catch(() => {});
}

/** 内核 appList 结果按 id merge 进缓存（水合与桥 listAppMessages 调用共用）；
 *  合并后按 createdAt 升序归位：水合期间本地新增的消息可能晚于内核快照尾部 */
export function mergeAppMessages(key: SpaceKey, convId: string, dtos: AppMessageDto[]): void {
  const space = spaces[key];
  if (!space) return;
  const localOnly = (space.appMessages[convId] ?? []).filter((m) => !dtos.some((d) => d.id === m.id));
  space.appMessages[convId] = [...dtos.map((d) => ({ ...d })), ...localOnly].sort(
    (a, b) => a.createdAt - b.createdAt
  );
}

// ---------- 内核事件订阅（与 network-status 消费同一 p2p-event 通道） ----------

function subscribeP2pEvents(): void {
  if (eventsSubscribed || !isTauri()) return;
  eventsSubscribed = true;
  void listenP2pEvents((event) => {
    if (event.kind === 'ChatReceived') onChatReceived(event.data);
    else if (event.kind === 'ChatStatus') onChatStatus(event.data);
    else if (event.kind === 'ConversationsSynced') hydrateConversations('personal');
    else if (event.kind === 'PeerConnected' || event.kind === 'PeerDisconnected') scheduleOnlineRefresh();
  }).catch(() => {});
}

/**
 * 对端上下线（PeerConnected/PeerDisconnected）后的 online 刷新：
 * 重拉已水合空间的会话列表，只 merge online 字段（不动 unreadCount 等本地态）。
 * 简单去抖：密集事件（启动时成批 PeerConnected）合并为一次刷新。
 */
let onlineRefreshTimer: ReturnType<typeof setTimeout> | undefined;

function scheduleOnlineRefresh(): void {
  if (onlineRefreshTimer !== undefined) return;
  onlineRefreshTimer = setTimeout(() => {
    onlineRefreshTimer = undefined;
    const api = messagesApi();
    if (!api) return;
    for (const key of Object.keys(spaces)) {
      const space = spaces[key];
      if (!space) continue;
      void api
        .listConversations(key)
        .then((dtos) => {
          const onlineById = new Map(dtos.map((dto) => [dto.id, dto.online]));
          for (const conv of space.conversations) {
            const online = onlineById.get(conv.id);
            if (online !== undefined) conv.online = online;
          }
        })
        .catch(() => {});
    }
  }, 300);
}

/**
 * 对端新消息：定位/创建会话，按 id 去重入列，维护未读与 updatedAt。
 * data.conversation 是内核回写本条消息之后的权威快照（unreadCount/updatedAt 已含
 * 本条；自设备同步来的 senderId='me' 消息内核不计未读），前端始终信任快照、不再
 * 本地 +1；仅活跃会话保持清零 + markRead。
 */
export function onChatReceived(data: { spaceKey: string; conversation: Conversation; message: ChatMessage }): void {
  const key = data.spaceKey;
  const space = ensureSpace(key);
  let conv =
    findConversation(space, data.conversation.id) ??
    space.conversations.find((c) => c.id === `dm:${data.message.senderId}`);
  if (!conv) {
    conv = { ...data.conversation };
    space.conversations.push(conv);
  }
  const list = (space.messages[conv.id] ??= []);
  if (!list.some((m) => m.id === data.message.id)) list.push({ ...data.message });
  conv.updatedAt = data.conversation.updatedAt;
  conv.online = data.conversation.online;
  if (activeConversation[key] === conv.id) {
    conv.unreadCount = 0;
    void messagesApi()
      ?.markRead(key, conv.id)
      .catch(() => {});
  } else {
    conv.unreadCount = data.conversation.unreadCount;
  }
  // bot 会话消息无需前端中继：内核在落库处直接分发给插件后台运行时
  // （QuickJS 沙箱，plugin_system.md「后台运行时」），覆盖本机与多设备回同步两路径
}

/** 消息状态事件：对方已读（peerRead）/ 对方撤回（recalled）/ 发送状态流转（status） */
function onChatStatus(data: {
  spaceKey: string;
  convId: string;
  messageId?: string;
  status?: MessageStatus;
  recalled?: boolean;
  peerRead?: boolean;
}): void {
  const space = spaces[data.spaceKey];
  if (!space) return;
  const list = space.messages[data.convId] ?? [];
  if (data.peerRead) {
    for (const msg of list) {
      if (msg.senderId === 'me' && (msg.status === 'sent' || msg.status === 'delivered')) msg.status = 'read';
    }
  }
  if (data.messageId && data.recalled) {
    const msg = list.find((m) => m.id === data.messageId);
    if (msg) msg.recalled = true;
  }
  if (data.messageId && data.status) {
    const msg = list.find((m) => m.id === data.messageId);
    if (msg && !msg.recalled) msg.status = data.status;
  }
}

function findConversation(space: SpaceData, convId: string): Conversation | undefined {
  return space.conversations.find((c) => c.id === convId);
}

/** 会话列表：置顶优先（按置顶时间倒序），其余按最后消息时间倒序（§2.3） */
export function listConversations(key: SpaceKey): Conversation[] {
  const list = [...ensureSpace(key).conversations];
  return list.sort((a, b) => {
    if ((a.pinnedAt > 0) !== (b.pinnedAt > 0)) return a.pinnedAt > 0 ? -1 : 1;
    if (a.pinnedAt > 0 && b.pinnedAt > 0) return b.pinnedAt - a.pinnedAt;
    return b.updatedAt - a.updatedAt;
  });
}

export function getConversation(key: SpaceKey, convId: string): Conversation | undefined {
  return findConversation(ensureSpace(key), convId);
}

export function getMessages(key: SpaceKey, convId: string): ChatMessage[] {
  const space = ensureSpace(key);
  hydrateMessages(key, convId);
  return space.messages[convId] ?? [];
}

/** 应用会话消息（首次访问触发 appList 水合，与 getMessages 同模式） */
export function getAppMessages(key: SpaceKey, convId: string): AppMessageDto[] {
  const space = ensureSpace(key);
  hydrateMessages(key, convId);
  return space.appMessages[convId] ?? [];
}

/** 应用会话最新摘要（会话列表预览；无消息时空串） */
export function lastAppSummary(key: SpaceKey, convId: string): string {
  const list = getAppMessages(key, convId);
  return list.length > 0 ? list[list.length - 1].summary : '';
}

/**
 * 应用消息就地入账（§20）：桥 dispatcher/系统通知经壳层服务（plugin/messages.ts）
 * 写入内核后调用，保证打开的会话与会话列表实时刷新，不必等下次水合。
 * 未读语义与 onChatReceived 同口径：活跃会话清零并回读，否则本地 +1
 * （内核侧已权威计数，下次水合自动对齐）。
 */
export function ingestAppMessage(key: SpaceKey, dto: AppMessageDto): void {
  const space = ensureSpace(key);
  const convId = `${APP_CONV_PREFIX}${dto.pluginId}`;
  let conv = findConversation(space, convId);
  if (!conv) {
    conv = {
      id: convId,
      kind: 'app',
      title: dto.pluginId,
      peerId: dto.pluginId,
      unreadCount: 0,
      pinnedAt: 0,
      muted: false,
      online: false,
      draft: '',
      updatedAt: dto.createdAt
    };
    space.conversations.push(conv);
  }
  conv.updatedAt = Math.max(conv.updatedAt, dto.createdAt);
  const list = (space.appMessages[convId] ??= []);
  if (list.some((m) => m.id === dto.id)) return;
  list.push({ ...dto });
  if (activeConversation[key] === convId) {
    conv.unreadCount = 0;
    const stored = list.find((m) => m.id === dto.id);
    if (stored) stored.read = true;
    void messagesApi()
      ?.appMarkRead(key, dto.pluginId)
      .catch(() => {});
  } else {
    conv.unreadCount += 1;
  }
}

// ---- 非 Tauri 环境的应用消息内存镜像（与内核 §20 语义对齐，mock 链路可演示） ----

const APP_SUMMARY_MAX_CHARS = 200;
const APP_MSG_RATE_LIMIT = 10;
const APP_MSG_RATE_WINDOW_MS = 60_000;
/** 应用消息 pluginId 白名单（与内核 §20.1 同规格，错误串前缀一致：invalid-plugin-id） */
const APP_PLUGIN_ID_PATTERN = /^[a-z0-9][a-z0-9-]{0,63}$/;
/** 限流记账：key = `${spaceKey}\n${pluginId}` → 窗口内写入时间戳（内存态，进程重启清零） */
const appRateLog = new Map<string, number[]>();

/**
 * 内存版应用消息写入：校验链（按序，先于限流，与内核口径一致）——
 * summary 非空且 ≤200（missing-summary/summary-too-long）→ pluginId 字符集
 * （invalid-plugin-id）→ 限流（rate-limited；内置 system 会话豁免，同内核
 * message_app_send：限流防插件刷会话，system 为壳层可信写入方）
 */
export function sendAppMessageLocal(
  key: SpaceKey,
  pluginId: string,
  payload: Record<string, unknown>,
  card?: AppMessageCardDto
): AppMessageDto {
  const summary = typeof payload.summary === 'string' ? payload.summary.trim() : '';
  if (!summary) {
    throw new Error('missing-summary: app message payload requires a non-empty summary');
  }
  if (summary.length > APP_SUMMARY_MAX_CHARS) {
    throw new Error('summary-too-long: app message summary exceeds 200 chars');
  }
  if (!APP_PLUGIN_ID_PATTERN.test(pluginId)) {
    throw new Error('invalid-plugin-id');
  }
  const now = Date.now();
  if (pluginId !== SYSTEM_APP_PLUGIN_ID) {
    const rateKey = `${key}\n${pluginId}`;
    const windowLog = (appRateLog.get(rateKey) ?? []).filter((ts) => now - ts < APP_MSG_RATE_WINDOW_MS);
    if (windowLog.length >= APP_MSG_RATE_LIMIT) {
      throw new Error('rate-limited: app message rate limit exceeded (10/60s)');
    }
    windowLog.push(now);
    appRateLog.set(rateKey, windowLog);
  }
  const dto: AppMessageDto = {
    id: `m${now}-${++seq}`,
    pluginId,
    summary,
    payload,
    createdAt: now,
    status: 'local',
    read: false,
    ...(card ? { card } : {})
  };
  ingestAppMessage(key, dto);
  return dto;
}

export function lastMessage(key: SpaceKey, convId: string): ChatMessage | undefined {
  const list = getMessages(key, convId);
  return list[list.length - 1];
}

/** 打开会话：记录当前会话并清零未读 */
export function openConversation(key: SpaceKey, convId: string): void {
  ensureSpace(key);
  activeConversation[key] = convId;
  markRead(key, convId);
}

export function closeConversation(key: SpaceKey): void {
  delete activeConversation[key];
}

export function markRead(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (conv) conv.unreadCount = 0;
  const pluginId = appConversationPluginId(convId);
  if (pluginId !== null) {
    // 应用会话：消息 read 批量置真 + 内核 appMarkRead（§20.3）
    for (const msg of spaces[key]?.appMessages[convId] ?? []) msg.read = true;
    void messagesApi()
      ?.appMarkRead(key, pluginId)
      .catch(() => {});
    return;
  }
  void messagesApi()
    ?.markRead(key, convId)
    .catch(() => {});
}

export function setDraft(key: SpaceKey, convId: string, draft: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (conv) conv.draft = draft;
  void messagesApi()
    ?.setDraft(key, convId, draft)
    .catch(() => {});
}

export function togglePin(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (conv) conv.pinnedAt = conv.pinnedAt > 0 ? 0 : Date.now();
  void messagesApi()
    ?.togglePin(key, convId)
    .catch(() => {});
}

export function toggleMute(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (conv) conv.muted = !conv.muted;
  void messagesApi()
    ?.toggleMute(key, convId)
    .catch(() => {});
}

/** 清空聊天记录：仅删本地消息，保留会话入口（§5.1） */
export function clearMessages(key: SpaceKey, convId: string): void {
  const space = ensureSpace(key);
  space.messages[convId] = [];
  const conv = findConversation(space, convId);
  if (conv) conv.unreadCount = 0;
  void messagesApi()
    ?.clear(key, convId)
    .catch(() => {});
}

/** 删除会话：仅删除列表入口，消息随会话一并移除（§5.1；应用会话走 appDeleteConversation，§20.7） */
export function deleteConversation(key: SpaceKey, convId: string): void {
  const space = ensureSpace(key);
  space.conversations = space.conversations.filter((c) => c.id !== convId);
  delete space.messages[convId];
  delete space.appMessages[convId];
  if (activeConversation[key] === convId) delete activeConversation[key];
  const pluginId = appConversationPluginId(convId);
  if (pluginId !== null) {
    void messagesApi()
      ?.appDeleteConversation(key, pluginId)
      .catch(() => {});
    return;
  }
  void messagesApi()
    ?.deleteConversation(key, convId)
    .catch(() => {});
}

/** 找到或创建与 peerId 的 1:1 会话（通讯录「发送消息」跳转用），返回会话 id（确定性 `dm:{peerId}`） */
export function ensureDirectConversation(key: SpaceKey, peerId: string, title: string): string {
  const space = ensureSpace(key);
  const existing = space.conversations.find((c) => c.kind === 'direct' && c.peerId === peerId);
  if (existing) return existing.id;
  const conv = makeConversation({ kind: 'direct', title, peerId, updatedAt: Date.now() });
  space.conversations.push(conv);
  void messagesApi()
    ?.ensureDirect(key, peerId, title)
    .then((dto) => {
      // 内核回传的权威字段（online 等）merge 进本地会话
      const local = findConversation(space, conv.id);
      if (local) Object.assign(local, dto);
    })
    .catch(() => {});
  return conv.id;
}

function setStatus(space: SpaceData, convId: string, messageId: string, status: MessageStatus): void {
  const msg = space.messages[convId]?.find((m) => m.id === messageId);
  if (msg && !msg.recalled) msg.status = status;
}

/** 发送文本消息：本地乐观入列（status 'sending'，含 URL 时上诚实占位卡片），
 *  内核落库后回写最终状态与抓取到的链接预览（dto.link 存在才替换占位；
 *  不存在则保留诚实占位），失败置 'failed' */
export function sendText(key: SpaceKey, convId: string, text: string, quote?: QuoteRef): ChatMessage | undefined {
  const space = ensureSpace(key);
  const conv = findConversation(space, convId);
  if (!conv) return undefined;
  const message: ChatMessage = {
    id: `m${Date.now()}-${++seq}`,
    senderId: 'me',
    senderName: '我',
    type: 'text',
    content: text,
    createdAt: Date.now(),
    status: 'sending',
    recalled: false,
    quote
  };
  const url = /https?:\/\/[^\s]+/.exec(text)?.[0];
  if (url) message.link = buildLinkPreview(url);
  (space.messages[convId] ??= []).push(message);
  conv.updatedAt = message.createdAt;
  conv.draft = '';
  // bot 会话：内核跳过 P2P 投递并在落库处直接分发给插件后台运行时处理，
  // 前端发送即完成，无中继/无消费者兜底
  void messagesApi()
    ?.sendText(key, convId, message.id, text, quote)
    .then((dto) => {
      if (dto.status) setStatus(space, convId, message.id, dto.status);
      // 抓取到的真实元数据替换占位卡片；消息已撤回/被删则不回写。
      // siteName 空串（页面无 og:site_name）时回退占位白名单/域名，不覆盖成空
      if (dto.link) {
        const msg = space.messages[convId]?.find((m) => m.id === message.id);
        if (msg && !msg.recalled) {
          msg.link = { ...dto.link, siteName: dto.link.siteName || msg.link?.siteName || dto.link.domain };
        }
      }
    })
    .catch(() => setStatus(space, convId, message.id, 'failed'));
  return message;
}

/** 发送失败重发：本地置 'sending'，内核重发后回写状态，失败置回 'failed' */
export function resendMessage(key: SpaceKey, convId: string, messageId: string): void {
  const space = ensureSpace(key);
  setStatus(space, convId, messageId, 'sending');
  void messagesApi()
    ?.resend(key, convId, messageId)
    .then((dto) => {
      if (dto.status) setStatus(space, convId, messageId, dto.status);
    })
    .catch(() => setStatus(space, convId, messageId, 'failed'));
}

/** 撤回：仅发送后 2 分钟内允许（§9.1），返回是否成功 */
export function recallMessage(key: SpaceKey, convId: string, messageId: string): boolean {
  const msg = ensureSpace(key).messages[convId]?.find((m) => m.id === messageId);
  if (!msg || msg.recalled || Date.now() - msg.createdAt > 2 * MIN) return false;
  msg.recalled = true;
  void messagesApi()
    ?.recall(key, convId, messageId)
    .catch(() => {});
  return true;
}

/** 删除消息：仅本地删除（§5.2） */
export function deleteMessage(key: SpaceKey, convId: string, messageId: string): void {
  const space = ensureSpace(key);
  space.messages[convId] = (space.messages[convId] ?? []).filter((m) => m.id !== messageId);
  void messagesApi()
    ?.deleteMessage(key, convId, messageId)
    .catch(() => {});
}

// ---------- 展示辅助 ----------

/** 会话列表最新内容缩略（§2.2） */
export function previewText(msg: ChatMessage | undefined): string {
  if (!msg) return '';
  if (msg.recalled) return '[消息已撤回]';
  switch (msg.type) {
    case 'image':
      return '[图片]';
    case 'file':
      return `[文件] ${msg.content}`;
    case 'link':
      return `[链接] ${msg.link?.title ?? msg.content}`;
    case 'voice':
      return '[语音]';
    case 'system':
      return `[系统通知] ${msg.content}`;
    default: {
      const text = msg.content.replace(/\s+/g, ' ').trim();
      return text.length > 30 ? `${text.slice(0, 30)}…` : text;
    }
  }
}

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** 会话时间（§2.2）：今天显示时间，昨天显示「昨天」，更早显示日期 */
export function formatConvTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const hhmm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  if (sameDay(d, now)) return hhmm;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(d, yesterday)) return '昨天';
  if (d.getFullYear() === now.getFullYear()) return `${d.getMonth() + 1}/${d.getDate()}`;
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

/** 聊天区时间分隔条：今天只显示时间，其余带日期 */
export function formatDividerTime(ts: number): string {
  const d = new Date(ts);
  const hhmm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const label = formatConvTime(ts);
  if (label === '昨天') return `昨天 ${hhmm}`;
  if (/^\d{2}:\d{2}$/.test(label)) return hhmm;
  return `${d.getMonth() + 1}月${d.getDate()}日 ${hhmm}`;
}

/** 未读聚合口径：免打扰会话不计；被屏蔽的应用会话同样抑制（屏蔽为本地持久化状态） */
function isUnreadSuppressed(key: SpaceKey, conv: Conversation): boolean {
  if (conv.muted) return true;
  const pluginId = appConversationPluginId(conv.id);
  return pluginId !== null && isAppConversationBlocked(key, pluginId);
}

/** 全部空间未读总数（免打扰/已屏蔽会话不计入角标，§5.1） */
export const totalUnread = computed(() => {
  let total = 0;
  for (const key of Object.keys(spaces)) {
    for (const conv of spaces[key].conversations) {
      if (!isUnreadSuppressed(key, conv)) total += conv.unreadCount;
    }
  }
  return total;
});

/** 某空间是否有未读消息（免打扰/已屏蔽会话不计；首次访问触发该空间水合，
 *  与 contactsOf 同模式——在 computed/渲染中调用即可保持响应式） */
export function hasUnreadMessages(key: SpaceKey): boolean {
  return ensureSpace(key).conversations.some((conv) => !isUnreadSuppressed(key, conv) && conv.unreadCount > 0);
}

/** 某空间未读总数（免打扰/已屏蔽会话不计；角标按空间隔离，不用全局 totalUnread） */
export function unreadCountOf(key: SpaceKey): number {
  let total = 0;
  for (const conv of ensureSpace(key).conversations) {
    if (!isUnreadSuppressed(key, conv)) total += conv.unreadCount;
  }
  return total;
}

// 未读数对外双通道（§7.1）：document.title 前缀 + 系统徽标（F4，
// 经 system-set-badge 命令桥到 dock/任务栏；非 Tauri 或平台不支持时静默跳过）
watch(
  totalUnread,
  (n) => {
    if (typeof document !== 'undefined') {
      document.title = n > 0 ? `(${n > 99 ? '…' : n}) 星火 Spark` : '星火 Spark';
    }
    // 运行期静默（平台不支持为 no-op）；开发期 bug（命令未注册/参数错）留线索
    void systemApi()?.setBadge(n).catch((e) => console.warn('[badge] setBadge 失败', e));
  },
  { immediate: true }
);
