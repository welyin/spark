/**
 * 消息页空状态跳转通讯录的意图哨兵（壳层 → 插件方向：经 viewBootstrap.cardData
 * 注入，pending-contact 消费；与壳层 components/contacts/open-intents 同值）。
 * ContactsPage 消费后按意图处理，不做联系人匹配。
 */
export const CONTACT_INTENT_BROWSE = '__browse__';
export const CONTACT_INTENT_ADD = '__add__';

import { hostSdk } from '../sdk-host';

/** 切到通讯录页并携带意图（壳层深链打开本插件主视图，意图经 cardData 注入）。
 *  插件 → 壳层方向导航统一走 sdk.navigation（沙箱 iframe 内 CustomEvent 不出
 *  浏览上下文，裸 dispatchEvent 是死通道） */
export function openContacts(intent: string): void {
  const navigation = hostSdk()?.navigation;
  if (!navigation) {
    console.warn('[spark-contacts] sdk.navigation 不可用，打开通讯录意图丢失：', intent);
    return;
  }
  void navigation.openPlugin({ pluginId: 'spark-contacts', cardData: { intent } }).catch((error) => {
    console.warn('[spark-contacts] 打开通讯录导航失败：', error);
  });
}

/** 打开/创建 1:1 会话（§5.3）：经 sdk.navigation.openChat 上行导航意图，
 *  壳层 dispatcher 校验参数后路由到 pending-chat 既有过渡链路（切到消息页
 *  找到或创建会话并选中）。name 为首建会话的兜底标题，conversationId 用于
 *  定位已存在的会话。 */
export function openChat(detail: { rootId: string; name?: string; conversationId?: string }): void {
  const navigation = hostSdk()?.navigation;
  if (!navigation) {
    console.warn('[spark-contacts] sdk.navigation 不可用，发消息意图丢失：', detail.rootId);
    return;
  }
  void navigation.openChat(detail).catch((error) => {
    console.warn('[spark-contacts] 发消息导航失败：', error);
  });
}
