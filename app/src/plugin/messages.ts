/**
 * 应用消息壳层服务（p2p-messages.md §20，服务号模型）。
 *
 * 两个调用方共用同一入口：
 * - 桥 dispatcher（插件经 sdk.messages 写入/读取，pluginId/space 由桥注入）；
 * - 壳层系统通知（内置 system 应用会话 `app:system`，如插件安装/升级成功）。
 *
 * Tauri 环境走内核命令（唯一真源），写后就地 ingest 进 stores/messages 缓存，
 * 保证打开的会话与会话列表实时刷新；非 Tauri（纯浏览器/单测）由 stores/messages
 * 内存态镜像内核语义（summary 校验 + 限流，错误串前缀一致），链路可演示。
 */
import { isTauri, type AppMessageCardDto, type AppMessageDto, type ElectronAPI } from '../api';
import {
  getAppMessages,
  ingestAppMessage,
  markRead as markConversationRead,
  mergeAppMessages,
  sendAppMessageLocal
} from '../stores/messages';
import { SYSTEM_APP_PLUGIN_ID } from '../stores/app-conversations';

/** 内核消息接口：仅 Tauri 环境可用（与 stores/messages 同口径守卫） */
function messagesApi(): ElectronAPI['messages'] | undefined {
  if (!isTauri()) return undefined;
  return (window as unknown as { electronAPI?: ElectronAPI }).electronAPI?.messages;
}

/** 应用会话 id（§20.1：`app:{pluginId}`，与内核 app_conversation_id 同约定） */
export function appConversationId(pluginId: string): string {
  return `app:${pluginId}`;
}

/**
 * 写入应用消息：payload 必须含非空 summary（内核/内存镜像同口径校验，
 * 错误前缀 missing-summary/summary-too-long/invalid-plugin-id；每插件每会话
 * 限流 10 条/60s，超限 rate-limited；内置 system 会话豁免限流，见内核
 * message_app_send 注释）。
 */
export async function sendAppMessage(
  spaceKey: string,
  pluginId: string,
  payload: Record<string, unknown>,
  card?: AppMessageCardDto
): Promise<AppMessageDto> {
  const api = messagesApi();
  if (!api) {
    return sendAppMessageLocal(spaceKey, pluginId, payload, card);
  }
  const dto = await api.appSend(spaceKey, pluginId, payload, card);
  ingestAppMessage(spaceKey, dto);
  return dto;
}

/** 应用消息列表（时间升序；Tauri 下回写缓存，打开的会话立即可见） */
export async function listAppMessages(spaceKey: string, pluginId: string): Promise<AppMessageDto[]> {
  const api = messagesApi();
  if (!api) {
    return [...getAppMessages(spaceKey, appConversationId(pluginId))];
  }
  const dtos = await api.appList(spaceKey, pluginId);
  mergeAppMessages(spaceKey, appConversationId(pluginId), dtos);
  return dtos;
}

/**
 * 清零应用会话未读（本地缓存与内核一并清零，语义与人际会话一致）。
 * Tauri 下透传内核调用结果：失败返回 success:false（本地缓存不清零、
 * 保留未读，调用方可提示/重试）；成功才同步清零本地缓存。
 */
export async function markAppMessagesRead(spaceKey: string, pluginId: string): Promise<{ success: boolean }> {
  const api = messagesApi();
  if (!api) {
    markConversationRead(spaceKey, appConversationId(pluginId));
    return { success: true };
  }
  try {
    await api.appMarkRead(spaceKey, pluginId);
  } catch {
    return { success: false };
  }
  markConversationRead(spaceKey, appConversationId(pluginId));
  return { success: true };
}

// ------------------------------------------------------------------
// 系统通知（内置 system 应用会话；spaceId 隔离与人际会话一致）
// 本波只接「插件安装/升级成功」一条真实通知源作为样板，其余通知源待接（TODO.md）。
// ------------------------------------------------------------------

/** 插件安装成功系统通知（fire-and-forget：通知写入失败不影响安装主流程，留 warn 线索） */
export function notifyPluginInstalled(spaceKey: string, pluginName: string): void {
  void sendAppMessage(spaceKey, SYSTEM_APP_PLUGIN_ID, {
    summary: `应用「${pluginName}」安装成功，启用后即可使用`,
    kind: 'plugin-installed',
    pluginName
  }).catch((error) => console.warn('[plugin/messages] 插件安装系统通知写入失败', error));
}

/** 插件升级成功系统通知（fire-and-forget，同安装口径） */
export function notifyPluginUpgraded(spaceKey: string, pluginName: string): void {
  void sendAppMessage(spaceKey, SYSTEM_APP_PLUGIN_ID, {
    summary: `应用「${pluginName}」已更新到最新版本`,
    kind: 'plugin-upgraded',
    pluginName
  }).catch((error) => console.warn('[plugin/messages] 插件升级系统通知写入失败', error));
}
