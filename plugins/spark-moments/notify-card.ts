/**
 * 朋友圈插件（spark-moments）· message-card 视图入口（互动通知卡片）。
 *
 * 与 spark-example post-card.ts 同构：connectPluginBridge 握手（viewId 'notify-card'，
 * 须与 manifest.views 声明一致），SDK 写入全局注入点后挂载 NotifyCard。
 * cardData = 应用消息的 card.data 透传（cards.md §2.3 数据契约）。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import NotifyCard, { type NotifyCardData } from './NotifyCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapNotifyCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'notify-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(NotifyCard, { cardData: ctx.mount.cardData as NotifyCardData | undefined });
  app.mount(container);
}
