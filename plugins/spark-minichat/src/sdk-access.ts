/**
 * spark-minichat SDK 访问层：桥握手后绑定的 SDK/ctx 句柄 + 会话数据面。
 * 刻意保持最小——全部数据面只有 sdk.messages 的 conversations/list/send/
 * onNewMessage 四个调用（communication §六验收样例：最小面读写同一消息数据）。
 *
 * 身份钉死（评审 2026-10-07）：验收对照件，不上市场、不作产品演进
 * （catalog.md 表下注 + 随包 README.md）。
 *
 * 刻意省略项（最小但不误导；正式聊天插件必须做，正确做法见 README.md）：
 * - 打开会话不调 markConversationRead（未读角标在本样例永不收敛）；
 * - 不订阅 onStatus（撤回/已读/投递失败不实时反映，需手动重进会话）；
 * - 无失败重发与分页。
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
/** 发送失败提示（样例即规范：SDK 调用拒绝必须浮面，不静默冒泡为未处理异常） */
export const sendError = ref('');

/** 重读会话列表（启动与 onNewMessage 后收敛） */
export async function refreshConversations(): Promise<void> {
  conversations.value = await messages().conversations();
}

/**
 * 选中会话并读消息（窗口内全量）。
 * 刻意省略：不调 markConversationRead——未读角标在本样例永不收敛
 * （正式插件必须在打开会话时标记已读，见 README「刻意省略项」）。
 */
export async function openConversation(convId: string): Promise<void> {
  activeConvId.value = convId;
  activeMessages.value = await messages().list(convId);
}

/** 发文本消息（发送后重读本会话收敛）；失败置 sendError 提示，不静默 */
export async function sendText(text: string): Promise<void> {
  if (!activeConvId.value || !text.trim()) {
    return;
  }
  sendError.value = '';
  try {
    await messages().send(activeConvId.value, text.trim());
  } catch {
    sendError.value = '发送失败，请检查网络后重试';
    return;
  }
  activeMessages.value = await messages().list(activeConvId.value);
}

/**
 * 订阅新消息：命中当前会话则重读消息，同时刷新会话列表（未读/排序收敛）。
 * 刻意省略：不订阅 onStatus——撤回/已读/投递失败的状态流转不实时反映
 * （正式插件必须订阅并就地合并，见 README「刻意省略项」）。
 */
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
