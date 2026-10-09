/**
 * 公告通知插件（spark-announcement）· message-card 视图入口（公告卡片）。
 *
 * 与主视图入口（index.ts）同构：connectPluginBridge 握手拿 SDK 与运行上下文
 * （viewId 固定 'announce-card'，须与 manifest.views 声明一致），SDK 写入全局
 * 注入点后挂载 AnnounceCard 到宿主 #app 容器。卡片数据（ctx.mount.cardData）只
 * 携带引用 { announcementId, orgId }，正文经 docs 查询。
 *
 * 只导出引导函数、不在顶层自执行——作为独立 vite 入口产出
 * dist/views/announce-card.js，为壳层按 view 直载预留（当前壳层固定加载
 * views/main.js，由主入口按 __sparkPluginView 内部分发到本模块）。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import AnnounceCard from './AnnounceCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapAnnounceCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'announce-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(AnnounceCard, {
    cardData: ctx.mount.cardData as { announcementId?: string; orgId?: string } | undefined
  });
  app.mount(container);
}
