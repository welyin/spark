/**
 * 消息页空状态打开通讯录的意图哨兵。
 * 0.3 遗留已解：通讯录是空间插件（spark-contacts，docs/ui §八决策 1），不再是壳层 tab。
 * 打开走统一深链（services/deep-link），意图经 viewBootstrap.cardData 注入插件沙箱
 * （插件侧 pending-contact 消费），替代旧的 spark:open-contact tab 切换。
 */
import { openPluginDeepLink } from '../../services/deep-link';

export const CONTACT_INTENT_BROWSE = '__browse__';
export const CONTACT_INTENT_ADD = '__add__';

/** 打开空间通讯录插件并携带意图（browse=仅落地 / add=打开添加对话框） */
export function openContacts(intent: string): void {
  openPluginDeepLink({ pluginId: 'spark-contacts', cardData: { intent } });
}

/** 打开/创建 1:1 会话（App.vue 消费 `spark:open-chat`：记录请求并切到消息页，§5.3）。
 *  所有「去找他聊天」的入口（通讯录资料卡/新朋友面板/全局搜索）统一从这里派发，
 *  不再各自裸写 CustomEvent。name 为首建会话的兜底标题，conversationId 用于定位已存在的会话。 */
export function openChat(detail: { rootId: string; name?: string; conversationId?: string }): void {
  window.dispatchEvent(new CustomEvent('spark:open-chat', { detail }));
}
