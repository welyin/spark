/**
 * 通知编排（阶段四C，wiki architecture/p2p/android-notifications §2）：
 * 内核 ChatReceived 事件 → 判流（前台正在看该会话 / 会话免打扰跳过）→
 * 系统通知（Android 真弹；桌面命令侧 no-op，后续接 tauri-plugin-notification）。
 *
 * - 标题 = 会话名，正文 = 内容截断约 80 字符；稳定 id = convId（Rust/Kotlin
 *   侧 hash，同会话覆盖更新不堆叠）；
 * - 同会话 3 秒内的连续消息合并为一条通知（更新正文与计数），不逐条弹；
 * - 系统会话（sys:notice，组织邀请卡片/设备事件等）走泛化提醒：标题
 *   「Spark」、正文「你有一条新的系统消息」，不含具体内容（内容以系统
 *   会话内记录为准——通知是铃铛，留痕在会话）。
 */
import { listenP2pEvents } from '../api';
import { getConversation, isConversationActive, unreadCountOf } from './messages';
import type { SpaceKey } from './messages';

/** 系统会话 convId（core inbound_dm/org_invite.rs SYSTEM_CONV_ID 同值）。 */
const SYSTEM_CONV_ID = 'sys:notice';
/** 同会话通知合并防抖窗口（ms）。 */
const DEBOUNCE_MS = 3000;
/** 通知正文截断长度（约 80 字符）。 */
const BODY_MAX_CHARS = 80;

/** 每会话最近一次通知时刻（合并防抖）。 */
const lastNotifyAt = new Map<string, number>();

/** 本次 ChatReceived 是否应弹系统通知（判流 + 记录合一：返回 true 时登记
    本次时刻——调用方紧接着发通知，防抖窗口由此推进）。 */
export function shouldNotifyChat(spaceKey: SpaceKey, convId: string, muted: boolean, now: number): boolean {
  if (muted) return false;
  if (isConversationActive(spaceKey, convId)) return false;
  const last = lastNotifyAt.get(convId) ?? 0;
  if (now - last < DEBOUNCE_MS) return false;
  lastNotifyAt.set(convId, now);
  return true;
}

/** 初始化通知订阅（App.vue onMounted 调用一次；幂等）。 */
export function initNotify(): void {
  // 无 Tauri 运行时（单测/纯 Web）订阅即拒——静默跳过
  void listenP2pEvents((event) => {
    if (event.kind !== 'ChatReceived') return;
    const { spaceKey, conversation } = event.data as {
      spaceKey: SpaceKey;
      conversation: { id: string; title: string; muted: boolean };
      message: { content?: string };
    };
    const key = spaceKey;
    const now = Date.now();
    if (!shouldNotifyChat(key, conversation.id, conversation.muted, now)) return;
    if (conversation.id === SYSTEM_CONV_ID) {
      // 系统事件泛化提醒：不含具体内容（§2.2）
      void window.electronAPI?.system.notifyGeneric('Spark', '你有一条新的系统消息').catch(() => {});
      return;
    }
    const conv = getConversation(key, conversation.id);
    const body = (event.data as { message?: { content?: string } }).message?.content ?? '';
    void window.electronAPI?.system.notifyChat(
      key,
      conversation.id,
      conv?.title || conversation.title || '新消息',
      body.length > BODY_MAX_CHARS ? `${body.slice(0, BODY_MAX_CHARS)}…` : body,
      unreadCountOf(key),
    ).catch(() => {});
  }).catch(() => {});
}
