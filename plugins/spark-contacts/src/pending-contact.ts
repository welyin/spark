/**
 * 「打开联系人资料 / 添加朋友·成员」请求（插件侧消费端）。
 *
 * 穿透沙箱的通道（A19 遗留已解）：壳层经统一深链（services/deep-link）打开本插件时，
 * 把意图写进 viewBootstrap.cardData → 宿主注入 window.__sparkPluginView.cardData，
 * 插件握手后可读（见 index.ts bootstrap）。payload 形态：
 *   { intent: '__browse__' | '__add__' }        消息页空态「发起新会话/添加朋友」
 *   { rootId: '<64hex>' }                        全局搜索打开某联系人资料
 * 读取即消费（一次性）。
 */
import { ref } from 'vue';

export type PendingContact = { rootId: string };

/** 从注入的视图引导消费一次意图（无则 null）。CONTACT_INTENT 哨兵值映射为 rootId 承载。 */
function readBootstrapIntent(): PendingContact | null {
  const data = window.__sparkPluginView?.cardData as
    | { intent?: string; rootId?: string }
    | undefined;
  if (!data) {
    return null;
  }
  if (typeof data.intent === 'string' && data.intent) {
    return { rootId: data.intent };
  }
  if (typeof data.rootId === 'string' && data.rootId) {
    return { rootId: data.rootId };
  }
  return null;
}

/** 待消费请求：初值 = 深链注入的意图（无则 null） */
export const pendingContact = ref<PendingContact | null>(readBootstrapIntent());

/** 消费请求（取出并清空；无请求时返回 null） */
export function consumePendingContact(): PendingContact | null {
  const target = pendingContact.value;
  pendingContact.value = null;
  return target;
}

