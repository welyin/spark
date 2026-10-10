/**
 * 消息 store（设计 §2/§3/§5/§9）：内核真实数据 + 内存响应式缓存的接入层。
 *
 * 数据流：sdk.messages（经 sdk-host 同签名适配，A18/A19 communication §4.1
 * 数据面）为唯一真源，本文件维护按空间 key 隔离的响应式缓存（'personal'
 * 个人空间 / 'org:<orgId>' 组织空间；space 由桥绑定注入，适配层忽略 spaceKey
 * 实参）。所有读写仍收敛在本文件的 store 方法上（发送/置顶/免打扰/清空/
 * 删除/撤回等），组件零改动：
 * - 读：同步返回缓存；桥上下文首次访问时异步水合（conversations / list），
 *   完成后响应式自动刷新。
 * - 写：本地缓存同步更新（保持组件的同步语义），随后调内核持久化。
 *   破坏性/状态类写（置顶/免打扰/清空/删除会话/删除消息/撤回）内核未确认
 *   即回滚本地态并经 notifyWriteError 提示——不回滚则本地与内核持久分歧
 *   （删除会话内核失败后，下次水合以内核快照为准会把会话静默复活）。
 *   草稿/已读为自愈态（连续改写、下次水合或同步事件自然对齐），失败静默
 *   回滚不提示。乐观更新仅 sendText/resend 保留（有 'failed' 终态语义）。
 * - 水合 merge：内核快照为权威——请求发出前已在本地的条目以内核回包为准
 *   覆盖，仅保留请求在途期间本地新建的条目（乐观创建的会话/刚入列的消息）。
 * - 远端推送：经 listenP2pEvents（桥事件面）订阅 ChatReceived（新消息）/
 *   ChatStatus（已读/撤回/状态流转）事件就地合并进缓存。
 * 未绑定 SDK（vitest / 纯前端预览）不发任何调用，退化为纯内存 store。
 *
 * 链接预览（§6）：发送方壳层（src-tauri）抓取 OG/Twitter Card 元数据随消息
 * 携带投递，接收方只展示不访问 URL。发送时先上 `buildLinkPreview` 的诚实占位
 * （域名白名单站点名 + 空描述），抓取结果随 sendText 的 dto.link 回来后替换；
 * 非 Tauri/demo 环境就停留在诚实占位。
 */
import { computed, reactive } from 'vue';
import { boundSpaceKey, isTauri, listenP2pEvents, messagesApi } from './sdk-host';

/** 空间 key：个人空间为 'personal'，组织空间为 'org:<orgId>' */
export type SpaceKey = string;

export type MessageType = 'text' | 'image' | 'file' | 'link' | 'voice' | 'system';
/** 消息状态（设计 §3.3）：发送中/已发送/已送达/已读/发送失败 */
// 'streaming'：AI 流式回复的中间态（内核 bot_reply_stream_* 落 status='streaming'，
// 终态 delivered/failed）；其余与内核 MessageRecord.status 对齐
export type MessageStatus = 'sending' | 'sent' | 'delivered' | 'read' | 'failed' | 'streaming';

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
}

const MIN = 60_000;

/** 本地生成消息 id 的自增序号（id 形如 `m${Date.now()}-${seq}`，前端生成后随发送传入内核落库） */
let seq = 0;

/**
 * 写失败通知出口：store 不直接依赖 UI 组件库（保持 vitest 纯内存可测），
 * 由根视图（ChatApp）注入 ElMessage.error；缺省退化为 console.warn。
 */
let notifyWriteError: (message: string) => void = (message) => console.warn(`[spark-chat] ${message}`);

export function setWriteErrorNotifier(notify: (message: string) => void): void {
  notifyWriteError = notify;
}

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

/** 空间 key 约定：个人空间为 'personal'，组织空间为 'org:{orgId}' */
export function spaceKeyOf(space: { type: string; id: string }): string {
  return space.type === 'org' ? `org:${space.id}` : 'personal';
}

function ensureSpace(key: SpaceKey): SpaceData {
  if (!spaces[key]) {
    spaces[key] = { conversations: [], messages: {} };
    subscribeP2pEvents();
    hydrateConversations(key);
  }
  return spaces[key];
}

/**
 * 登录态切换时清空消息缓存（测试隔离同用）。运行态跨账号清理依赖壳层
 * 在登出/切换账号时销毁并重建插件 iframe（模块级单例随浏览上下文消亡）——
 * 该前提见评审待核登记（wiki/product/todo.md）。
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

/**
 * 首次进入空间时拉取会话列表水合缓存。内核快照为权威：请求发出前已在本地的
 * 会话以内核回包为准覆盖（内核未确认的删除不会借「保留本地新建」复活）；
 * 仅保留请求在途期间本地新建的会话（乐观创建/新消息事件）。
 */
function hydrateConversations(key: SpaceKey): void {
  const api = messagesApi();
  if (!api) return;
  const knownIds = new Set((spaces[key]?.conversations ?? []).map((c) => c.id));
  void api
    .listConversations(key)
    .then((dtos) => {
      const space = spaces[key];
      if (!space) return;
      const localOnly = space.conversations.filter((c) => !knownIds.has(c.id) && !dtos.some((d) => d.id === c.id));
      space.conversations = [...dtos.map((d) => ({ ...d })), ...localOnly];
    })
    .catch(() => {});
}

/**
 * 首次读取某会话消息时拉取历史水合缓存。与 hydrateConversations 同口径：
 * 内核快照为权威，仅保留请求在途期间本地新入列的消息（乐观发送等）。
 */
function hydrateMessages(key: SpaceKey, convId: string): void {
  const loadedKey = `${key}\n${convId}`;
  if (hydratedMessages.has(loadedKey)) return;
  hydratedMessages.add(loadedKey);
  const api = messagesApi();
  if (!api) return;
  const knownIds = new Set((spaces[key]?.messages[convId] ?? []).map((m) => m.id));
  void api
    .listMessages(key, convId)
    .then((dtos) => {
      const space = spaces[key];
      if (!space) return;
      const localOnly = (space.messages[convId] ?? []).filter(
        (m) => !knownIds.has(m.id) && !dtos.some((d) => d.id === m.id)
      );
      space.messages[convId] = [...dtos.map((d) => ({ ...d })), ...localOnly];
    })
    .catch(() => {});
}

// ---------- 内核事件订阅（与 network-status 消费同一 p2p-event 通道） ----------

function subscribeP2pEvents(): void {
  if (eventsSubscribed || !isTauri()) return;
  eventsSubscribed = true;
  void listenP2pEvents((event) => {
    // 判别联合按 kind 收窄后 data 形状确定（与壳层 P2pEventDto 同口径）
    if (event.kind === 'ChatReceived') onChatReceived(event.data);
    else if (event.kind === 'ChatStatus') onChatStatus(event.data);
    // 空间键单一事实源：实例由桥按空间绑定，同步事件只刷新本空间
    else if (event.kind === 'ConversationsSynced') hydrateConversations(boundSpaceKey());
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

// ------------------------------------------------------------------
// 流式打字机渲染器：streaming 消息的同 id 覆盖不直接落全量 content，
// 而是把目标全文按字符逐帧推进显示——后端（codebuddy/ollama）给的常是
// 整段快照，逐字吐出实现"一字一字"上屏、便于等待与阅读。
// 每条流式消息一个渲染器实例（按 convId+messageId 索引），终态/新覆盖时
// 推进目标；render 回调把当前已显示前缀写回 store（显式替换数组引用）。
// ------------------------------------------------------------------

/** 每帧吐出的字符数：~50ms 一帧 × 3 字 ≈ 60 字/秒，接近自然阅读速度 */
const TYPEWRITER_CHARS_PER_TICK = 3;
const TYPEWRITER_TICK_MS = 50;

interface TypewriterState {
  /** 目标全文（后端最新快照/终态全文） */
  target: string;
  /** 当前已显示的前缀长度 */
  shown: number;
  timer: ReturnType<typeof setInterval> | null;
}

const typewriters = new Map<string, TypewriterState>();

function typewriterKey(convId: string, messageId: string): string {
  return `${convId}\n${messageId}`;
}

/**
 * 推进/创建某条流式消息的打字机：target 更新为最新全文，按帧逐字追加显示。
 * render(displayed) 由调用方提供，把当前应显示的前缀写回 store。
 */
function typewriterTarget(
  convId: string,
  messageId: string,
  target: string,
  render: (displayed: string) => void,
): void {
  const key = typewriterKey(convId, messageId);
  let state = typewriters.get(key);
  if (!state) {
    state = { target, shown: 0, timer: null };
    typewriters.set(key, state);
  } else if (target.length < state.shown) {
    // 目标变短（理论上不该发生——快照只增）：重置到新目标
    state.shown = 0;
  }
  state.target = target;

  if (state.timer) return; // 已在推进，target 更新即可（下一帧自动接续）
  state.timer = setInterval(() => {
    const s = typewriters.get(key);
    if (!s) return;
    if (s.shown >= s.target.length) {
      // 已显示完全部目标：暂停等下一波 target（不清 timer 槽——target 可能再更新）
      return;
    }
    s.shown = Math.min(s.shown + TYPEWRITER_CHARS_PER_TICK, s.target.length);
    render(s.target.slice(0, s.shown));
  }, TYPEWRITER_TICK_MS);
}

/**
 * 终态 flush：立刻显示目标全文并销毁渲染器（delivered/failed 时调用，
 * 防流式尾部字符在终态后又被吐出覆盖最终 content）。
 */
function typewriterFlush(convId: string, messageId: string): void {
  const key = typewriterKey(convId, messageId);
  const state = typewriters.get(key);
  if (!state) return;
  if (state.timer) clearInterval(state.timer);
  typewriters.delete(key);
}

/**
 * 对端/自设备新消息：定位/创建会话，按 id 去重入列，维护未读与 updatedAt。
 * data.conversation 是内核回写本条消息之后的权威快照（unreadCount/updatedAt 已含
 * 本条；每台设备独立追踪 unread_count，自设备同步来的消息也会计入未读），前端
 * 始终信任快照、不再本地 +1；仅活跃会话保持清零 + markRead。
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
  // 显式替换数组引用而非 push 变异：Tauri IPC 事件回调在 Vue 组件事件循环
  // 之外触发，push 变异的 Proxy 拦截可能不会可靠触发渲染（与 sendText 的
  // 用户事件上下文不同）。创建新数组引用确保 Vue computed 无条件检测到变更。
  const list = space.messages[conv.id] ?? [];
  const existingIdx = list.findIndex((m) => m.id === data.message.id);
  const incoming = data.message;

  // 流式打字机：streaming 中间态的同 id 覆盖不直接落全量 content，而是把目标
  // 全文交给逐字渲染器按字符推进——后端（codebuddy/ollama 快照）给的常是整段，
  // 逐字吐出实现"一字一字"上屏、便于阅读。终态（非 streaming）或首次插入直接落。
  if (incoming.status === 'streaming' && existingIdx >= 0) {
    typewriterTarget(conv.id, incoming.id, incoming.content ?? '', (displayed) => {
      const cur = space.messages[conv.id] ?? [];
      const idx = cur.findIndex((m) => m.id === incoming.id);
      if (idx < 0) return;
      const next = [...cur];
      next[idx] = { ...cur[idx], content: displayed, status: 'streaming' };
      space.messages[conv.id] = next;
    });
  } else if (existingIdx >= 0) {
    // 同 id 覆盖（终态 delivered/failed，或非流式同 id 更新）：flush 打字机残留
    // 再落最终 content——防流式尾部字符在终态后又被吐出覆盖
    typewriterFlush(conv.id, incoming.id);
    const next = [...list];
    next[existingIdx] = { ...incoming };
    space.messages[conv.id] = next;
  } else {
    space.messages[conv.id] = [...list, { ...incoming }];
  }
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

/** 该会话是否正被用户查看（阶段四C 通知编排的「前台正在看」判定）。 */
export function isConversationActive(key: SpaceKey, convId: string): boolean {
  return activeConversation[key] === convId;
}

/** 已读为自愈态：内核未确认时静默回滚未读数（下次水合/同步事件亦会对齐），不提示 */
export function markRead(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (!conv) return;
  const prev = conv.unreadCount;
  conv.unreadCount = 0;
  void messagesApi()
    ?.markRead(key, convId)
    .catch(() => {
      const cur = spaces[key]?.conversations.find((c) => c.id === convId);
      // 期间新到的消息会抬高未读：回滚取较大者，不吞新未读
      if (cur) cur.unreadCount = Math.max(cur.unreadCount, prev);
    });
}

/** 草稿为连续改写态：仅当用户未继续输入（本地值仍为本次写入）才回滚，不提示 */
export function setDraft(key: SpaceKey, convId: string, draft: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (!conv) return;
  const prev = conv.draft;
  conv.draft = draft;
  void messagesApi()
    ?.setDraft(key, convId, draft)
    .catch(() => {
      if (conv.draft === draft) conv.draft = prev;
    });
}

export function togglePin(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (!conv) return;
  const prev = conv.pinnedAt;
  conv.pinnedAt = prev > 0 ? 0 : Date.now();
  void messagesApi()
    ?.togglePin(key, convId)
    .catch(() => {
      const cur = spaces[key]?.conversations.find((c) => c.id === convId);
      if (cur) cur.pinnedAt = prev;
      notifyWriteError('置顶操作失败，请重试');
    });
}

export function toggleMute(key: SpaceKey, convId: string): void {
  const conv = findConversation(ensureSpace(key), convId);
  if (!conv) return;
  const prev = conv.muted;
  conv.muted = !prev;
  void messagesApi()
    ?.toggleMute(key, convId)
    .catch(() => {
      const cur = spaces[key]?.conversations.find((c) => c.id === convId);
      if (cur) cur.muted = prev;
      notifyWriteError('免打扰设置失败，请重试');
    });
}

/** 清空聊天记录：仅删本地消息，保留会话入口（§5.1）；内核未确认即回滚 */
export function clearMessages(key: SpaceKey, convId: string): void {
  const space = ensureSpace(key);
  const prevMessages = space.messages[convId] ?? [];
  const conv = findConversation(space, convId);
  const prevUnread = conv?.unreadCount ?? 0;
  space.messages[convId] = [];
  if (conv) conv.unreadCount = 0;
  void messagesApi()
    ?.clear(key, convId)
    .catch(() => {
      const cur = spaces[key];
      if (cur) {
        // 回滚与期间新到的消息合并（按 id 去重，原消息在前）
        const existing = cur.messages[convId] ?? [];
        const existingIds = new Set(existing.map((m) => m.id));
        cur.messages[convId] = [...prevMessages.filter((m) => !existingIds.has(m.id)), ...existing];
        const curConv = cur.conversations.find((c) => c.id === convId);
        if (curConv) curConv.unreadCount = Math.max(curConv.unreadCount, prevUnread);
      }
      notifyWriteError('清空聊天记录失败，请重试');
    });
}

/**
 * 删除会话：仅删除列表入口，消息随会话一并移除（§5.1）。
 * 内核未确认即回滚——否则本地已删、内核仍在，下次水合（内核快照为权威）
 * 会把会话静默复活且无任何提示。
 */
export function deleteConversation(key: SpaceKey, convId: string): void {
  const space = ensureSpace(key);
  const index = space.conversations.findIndex((c) => c.id === convId);
  const removed = index >= 0 ? space.conversations[index] : undefined;
  const removedMessages = space.messages[convId];
  const wasActive = activeConversation[key] === convId;
  space.conversations = space.conversations.filter((c) => c.id !== convId);
  delete space.messages[convId];
  if (wasActive) delete activeConversation[key];
  void messagesApi()
    ?.deleteConversation(key, convId)
    .catch(() => {
      const cur = spaces[key];
      if (cur && removed && !cur.conversations.some((c) => c.id === convId)) {
        cur.conversations.splice(Math.min(index, cur.conversations.length), 0, removed);
      }
      if (cur && removedMessages && !cur.messages[convId]) cur.messages[convId] = removedMessages;
      if (wasActive && !activeConversation[key]) activeConversation[key] = convId;
      notifyWriteError('删除会话失败，请重试');
    });
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

/** 撤回：仅发送后 2 分钟内允许（§9.1），返回是否成功；内核未确认即回滚 */
export function recallMessage(key: SpaceKey, convId: string, messageId: string): boolean {
  const msg = ensureSpace(key).messages[convId]?.find((m) => m.id === messageId);
  if (!msg || msg.recalled || Date.now() - msg.createdAt > 2 * MIN) return false;
  msg.recalled = true;
  void messagesApi()
    ?.recall(key, convId, messageId)
    .catch(() => {
      const cur = spaces[key]?.messages[convId]?.find((m) => m.id === messageId);
      if (cur) cur.recalled = false;
      notifyWriteError('撤回失败，请重试');
    });
  return true;
}

/** 删除消息：仅本地删除（§5.2）；内核未确认即回滚（与期间新到消息按时间归位） */
export function deleteMessage(key: SpaceKey, convId: string, messageId: string): void {
  const space = ensureSpace(key);
  const prev = space.messages[convId] ?? [];
  const removed = prev.find((m) => m.id === messageId);
  space.messages[convId] = prev.filter((m) => m.id !== messageId);
  void messagesApi()
    ?.deleteMessage(key, convId, messageId)
    .catch(() => {
      const cur = spaces[key];
      if (!cur || !removed) return;
      const existing = cur.messages[convId] ?? [];
      if (existing.some((m) => m.id === messageId)) return;
      cur.messages[convId] = [...existing, removed].sort((a, b) => a.createdAt - b.createdAt);
      notifyWriteError('删除消息失败，请重试');
    });
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

/** 全部空间未读总数（免打扰会话不计入角标，§5.1；供插件内列表 UI 使用） */
export const totalUnread = computed(() => {
  let total = 0;
  for (const key of Object.keys(spaces)) {
    for (const conv of spaces[key].conversations) {
      if (!conv.muted) total += conv.unreadCount;
    }
  }
  return total;
});

/** 某空间是否有未读消息（免打扰会话不计；首次访问触发该空间水合，
 *  与 contactsOf 同模式——在 computed/渲染中调用即可保持响应式） */
export function hasUnreadMessages(key: SpaceKey): boolean {
  return ensureSpace(key).conversations.some((conv) => !conv.muted && conv.unreadCount > 0);
}

/** 某空间未读总数（免打扰会话不计；角标按空间隔离，不用全局 totalUnread） */
export function unreadCountOf(key: SpaceKey): number {
  let total = 0;
  for (const conv of ensureSpace(key).conversations) {
    if (!conv.muted) total += conv.unreadCount;
  }
  return total;
}
