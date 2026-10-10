/**
 * 宿主接口本地类型（spark-chat 自包含）：与壳层 api/types.ts 的
 * ConversationDto / ChatMessageDto / QuoteRefDto 逐字段同形。
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

/**
 * 宿主消息接口（与壳层 ElectronAPI['messages'] 同签名）：store 的数据源形状。
 * 插件内由 sdk-host 以 sdk.messages 适配实现（space 已由桥绑定，spaceKey
 * 形参透传忽略）。
 *
 * 应用消息（服务号模型，p2p-messages.md §20）与系统徽标面不在此接口内：
 * v1 均为壳层职责（应用会话挂载区 / 壳层未读聚合），评审 U3/U4 已清算插件
 * 侧的桩与死代码；应用会话迁入聊天插件时需先补 SDK 契约（见 communication §4.2
 * 与 wiki/product/todo.md 登记）。
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
}
