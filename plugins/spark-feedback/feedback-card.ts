/**
 * 问题反馈应用（spark-feedback）· message-card 视图入口（反馈回执卡片）。
 *
 * 与主入口同构：connectPluginBridge 握手（viewId 固定 'feedback-card'，须与
 * manifest.views 声明一致）→ SDK 写全局注入点 → 挂载 FeedbackCard。
 * 当前壳层固定加载 views/main.js，由主入口按 window.__sparkPluginView 动态
 * import 本模块；本文件同时作为独立 vite 入口产出 dist/views/feedback-card.js，
 * 为壳层按 view 直载预留。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import FeedbackCard from './FeedbackCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapFeedbackCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'feedback-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(FeedbackCard, {
    cardData: ctx.mount.cardData as
      | { type?: string; title?: string; childAffairId?: string; targetAffairId?: string }
      | undefined
  });
  app.mount(container);
}
