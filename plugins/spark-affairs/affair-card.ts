/**
 * 公共议题客户端（spark-affairs）· message-card 视图入口（议题卡片）。
 *
 * 与主入口（index.ts）同构：connectPluginBridge 握手（viewId 固定 'affair-card'，
 * 须与 manifest.views 声明一致）→ SDK 写入全局注入点 → 挂载 AffairCard。
 * 当前壳层固定加载 views/main.js，由主入口按 window.__sparkPluginView 动态
 * import 本模块；本文件同时作为独立 vite 入口产出 dist/views/affair-card.js，
 * 为壳层按 view 直载预留（见 spark-example/post-card.ts 头注）。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import AffairCard from './AffairCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapAffairCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'affair-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(AffairCard, {
    cardData: ctx.mount.cardData as { affairId?: string } | undefined
  });
  app.mount(container);
}
