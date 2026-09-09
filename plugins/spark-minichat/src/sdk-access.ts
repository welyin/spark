/**
 * spark-minichat SDK 访问层：桥握手后绑定的 SDK/ctx 句柄 + 会话数据面。
 * 刻意保持最小——全部数据面只有 sdk.messages 的 conversations/list/send/
 * onNewMessage 四个调用（验收样例：最小面读写同一消息数据）。
 */
import { ref } from 'vue';
import type {
  PluginChatMessage,
  PluginContext,
  PluginConversation,
  PluginSDK
} from '../../../packages/plugin-sdk/src';

let _sdk: PluginSDK | null = null;
let _ctx: PluginContext | null = null;

/** 入口绑定（index.ts 握手成功后调用） */
export function bindMiniChatSdk(sdk: PluginSDK, ctx: PluginContext): void {
  _sdk = sdk;
  _ctx = ctx;
}

/** 当前绑定空间（桥 ready 下发的权威值；展示用） */
export function minichatSpace(): PluginContext['space'] {
  return _ctx?.space ?? { type: 'personal', id: 'personal' };
}

function messages() {
  if (!_sdk?.messages) {
    throw new Error('spark-minichat: sdk.messages 不可用（未绑定或未授权 messages:*）');
  }
  return _sdk.messages;
}

/** 会话列表（响应式缓存；refresh 重读收敛） */
export const conversations = ref<PluginConversation[]>([]);
/** 当前选中会话的消息（时间升序） */
export const activeMessages = ref<PluginChatMessage[]>([]);
/** 当前选中会话 id */
export const activeConvId = ref('');

/** 重读会话列表（启动与 onNewMessage 后收敛） */
export async function refreshConversations(): Promise<void> {
  conversations.value = await messages().conversations();
}

/** 选中会话并读消息（窗口内全量） */
export async function openConversation(convId: string): Promise<void> {
  activeConvId.value = convId;
  activeMessages.value = await messages().list(convId);
}

/** 发文本消息（发送后重读本会话收敛） */
export async function sendText(text: string): Promise<void> {
  if (!activeConvId.value || !text.trim()) {
    return;
  }
  await messages().send(activeConvId.value, text.trim());
  activeMessages.value = await messages().list(activeConvId.value);
}

/** 订阅新消息：命中当前会话则重读消息，同时刷新会话列表（未读/排序收敛） */
export async function subscribeNewMessages(): Promise<void> {
  await messages().onNewMessage((event) => {
    void refreshConversations();
    if (event.conversation.id === activeConvId.value) {
      void messages()
        .list(activeConvId.value)
        .then((list) => {
          activeMessages.value = list;
        });
    }
  });
}
