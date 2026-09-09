/**
 * 宿主接口本地类型（spark-chat 自包含）：与壳层 api/types.ts 的
 * ConversationDto / ChatMessageDto / QuoteRefDto / AppMessageDto 逐字段同形。
 *
 * 边界纪律（plugins/README.md）：插件禁止 import 壳层 app/src 任何模块——
 * 桥/宿主面 DTO 在插件内保留本地同形拷贝（结构类型天然兼容），壳层侧变更
 * 时按等语义纪律同步本文件。
 */
import type { PluginConversation, PluginChatMessage, PluginQuoteRef } from '../../../packages/plugin-sdk/src';

/** 会话 DTO（与壳层 ConversationDto 同形；与 SDK PluginConversation 同源） */
export type ConversationDto = PluginConversation;

/** 聊天消息 DTO（与壳层 ChatMessageDto 同形；与 SDK PluginChatMessage 同源） */
export type ChatMessageDto = PluginChatMessage;

/** 引用回复片段（与壳层 QuoteRefDto 同形） */
export type QuoteRefDto = PluginQuoteRef;

/** 应用消息卡片（与壳层 AppMessageCardDto 同形，p2p-messages.md §20.2） */
export interface AppMessageCardDto {
  viewId: string;
  data?: unknown;
}

/** 应用消息（服务号模型；本地生成、本地消费，状态恒 'local'，无 delivered 语义） */
export interface AppMessageDto {
  id: string;
  pluginId: string;
  /** 纯文本摘要（trim 后的 payload.summary；未装插件时壳层原生渲染此字段） */
  summary: string;
  /** 插件自描述 JSON（必须含非空 summary 字段，否则内核拒绝写入） */
  payload: Record<string, unknown>;
  card?: AppMessageCardDto;
  createdAt: number;
  status: 'local';
  read: boolean;
}

/**
 * 宿主消息接口（与壳层 ElectronAPI['messages'] 同签名）：store 的数据源形状。
 * 插件内由 sdk-host 以 sdk.messages 适配实现（space 已由桥绑定，spaceKey
 * 形参透传忽略）；应用消息面 v1 为桩（壳层挂载区职责，communication §4.2）。
 */
export interface HostMessagesApi {
  listConversations: (spaceKey: string) => Promise<ConversationDto[]>;
  listMessages: (spaceKey: string, convId: string) => Promise<ChatMessageDto[]>;
  ensureDirect: (spaceKey: string, peerId: string, title: string) => Promise<ConversationDto>;
  sendText: (spaceKey: string, convId: string, messageId: string, text: string, quote?: QuoteRefDto) => Promise<ChatMessageDto>;
  resend: (spaceKey: string, convId: string, messageId: string) => Promise<ChatMessageDto>;
  recall: (spaceKey: string, convId: string, messageId: string) => Promise<{ success: boolean }>;
  deleteMessage: (spaceKey: string, convId: string, messageId: string) => Promise<{ success: boolean }>;
  markRead: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  setDraft: (spaceKey: string, convId: string, draft: string) => Promise<{ success: boolean }>;
  togglePin: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  toggleMute: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  clear: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  deleteConversation: (spaceKey: string, convId: string) => Promise<{ success: boolean }>;
  // 应用消息（服务号模型，p2p-messages.md §20）：v1 壳层挂载区呈现，插件内为桩
  appSend: (spaceKey: string, pluginId: string, payload: Record<string, unknown>, card?: AppMessageCardDto) => Promise<AppMessageDto>;
  appList: (spaceKey: string, pluginId: string) => Promise<AppMessageDto[]>;
  appMarkRead: (spaceKey: string, pluginId: string) => Promise<{ success: boolean }>;
  appDeleteConversation: (spaceKey: string, pluginId: string) => Promise<{ success: boolean }>;
}

/** 宿主系统接口（与壳层 ElectronAPI['system'] 的插件消费面子集同签名） */
export interface HostSystemApi {
  /** 未读角标 → 系统徽标（dock/任务栏）；平台不支持时静默 */
  setBadge: (count: number) => Promise<void>;
}
