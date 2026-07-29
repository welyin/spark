/**
 * 跨页面「打开会话」请求（ui-contacts §5.3）。
 *
 * 通讯录/应用市场等页面请求打开与某人的 1:1 聊天：写入本模块并把 rail
 * 切到消息页（App.vue 监听 `spark:open-chat` 事件完成这两步），
 * MessagesPage 挂载/监听后消费该请求，找到或创建会话并选中。
 */
import { ref } from 'vue';

export interface PendingChat {
  rootId: string;
  /** 展示名（备注名优先）；为空时消息页回退用 RootID 缩写 */
  name: string;
  /** 已存在的会话 id（全局搜索选中已有会话时传入，避免按 peerId 重复创建） */
  conversationId?: string;
}

/** 待消费的打开会话请求；消息页消费后置空 */
export const pendingChat = ref<PendingChat | null>(null);

export function requestOpenChat(peer: PendingChat): void {
  pendingChat.value = peer;
}

/** 取出并清空当前请求（无请求时返回 null） */
export function consumePendingChat(): PendingChat | null {
  const peer = pendingChat.value;
  pendingChat.value = null;
  return peer;
}
