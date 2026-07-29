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
 */
import { computed, reactive, watch } from 'vue';
import { isTauri, listenP2pEvents, type ElectronAPI } from '../api';

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
  /** direct=1:1 单聊；system=系统通知/组织公告（设计 §8.3） */
  kind: 'direct' | 'system';
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

// TODO: 真实实现应由发送方本地抓取 OG/Twitter Card 元数据（§6.4），此处按域名映射生成
export function buildLinkPreview(url: string): LinkPreview {
  let domain = url;
  try {
    domain = new URL(url).hostname.replace(/^www\./, '');
  } catch {
    // 非法 URL 时原样展示
  }
  const siteName = KNOWN_SITES[domain] ?? domain;
  return {
    url,
    domain,
    siteName,
    title: `${siteName}：示例页面标题`,
    description: '这是链接预览的示例描述。真实环境中由发送方抓取网页元数据生成，接收方只展示不主动访问。'
  };
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

export function spaceKeyOf(space: { type: 'personal' } | { type: 'org'; orgId: string }): SpaceKey {
  return space.type === 'org' ? `org:${space.orgId}` : 'personal';
}

function ensureSpace(key: SpaceKey): SpaceData {
  if (!spaces[key]) {
    spaces[key] = { conversations: [], messages: {} };
    subscribeP2pEvents();
    hydrateConversations(key);
  }
  return spaces[key];
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

/** 首次读取某会话消息时拉取历史水合缓存；按 id merge，保留水合期间本地新增的消息 */
function hydrateMessages(key: SpaceKey, convId: string): void {
  const loadedKey = `${key}\n${convId}`;
  if (hydratedMessages.has(loadedKey)) return;
  hydratedMessages.add(loadedKey);
  const api = messagesApi();
  if (!api) return;
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

// ---------- 内核事件订阅（与 network-status 消费同一 p2p-event 通道） ----------

function subscribeP2pEvents(): void {
  if (eventsSubscribed || !isTauri()) return;
  eventsSubscribed = true;
  void listenP2pEvents((event) => {
    if (event.kind === 'ChatReceived') onChatReceived(event.data);
    else if (event.kind === 'ChatStatus') onChatStatus(event.data);
  }).catch(() => {});
}

/** 对方发来的新消息：定位/创建会话，按 id 去重入列，维护未读与 updatedAt */
function onChatReceived(data: { spaceKey: string; conversation: Conversation; message: ChatMessage }): void {
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
  conv.updatedAt = data.message.createdAt;
  if (activeConversation[key] === conv.id) {
    conv.unreadCount = 0;
    void messagesApi()
      ?.markRead(key, conv.id)
      .catch(() => {});
  } else {
    conv.unreadCount += 1;
  }
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

/** 删除单聊会话：仅删除列表入口，消息随会话一并移除（§5.1） */
export function deleteConversation(key: SpaceKey, convId: string): void {
  const space = ensureSpace(key);
  space.conversations = space.conversations.filter((c) => c.id !== convId);
  delete space.messages[convId];
  if (activeConversation[key] === convId) delete activeConversation[key];
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

/** 发送文本消息：本地乐观入列（status 'sending'），内核落库后回写最终状态，失败置 'failed' */
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
  void messagesApi()
    ?.sendText(key, convId, message.id, text, quote)
    .then((dto) => {
      if (dto.status) setStatus(space, convId, message.id, dto.status);
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

/** 全部空间未读总数（免打扰会话不计入角标，§5.1） */
export const totalUnread = computed(() => {
  let total = 0;
  for (const key of Object.keys(spaces)) {
    for (const conv of spaces[key].conversations) {
      if (!conv.muted) total += conv.unreadCount;
    }
  }
  return total;
});

// TODO: 标题未读数（§7.1）已实现；系统托盘角标与任务栏徽标待真实通知能力
watch(
  totalUnread,
  (n) => {
    if (typeof document === 'undefined') return;
    document.title = n > 0 ? `(${n > 99 ? '…' : n}) 星火 Spark` : '星火 Spark';
  },
  { immediate: true }
);
