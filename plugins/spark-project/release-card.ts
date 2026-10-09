/**
 * 项目（议题）插件（spark-project）· message-card 视图入口（发布卡片）。
 *
 * 组合纪律（档一-2）：版本卡片唯一推送源是发布管理件（库件形态经
 * ReleaseManagerService 推送 viewId='release-card' 卡片）；组合形态下
 * 卡片在本插件 manifest 的同名视图渲染——本入口即该渲染面。卡片 data
 * 只携带引用 { releaseId, orgId }（releaseRef = 发布单记录 id，档三-24），
 * 发布单经组合服务（sdk.docs，内核按本插件域隔离）重读。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ReleaseCardView from './ReleaseCardView.vue';
import manifestJson from './manifest.json';

export async function bootstrapReleaseCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'release-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(ReleaseCardView, {
    cardData: ctx.mount.cardData as { releaseId?: string; orgId?: string } | undefined
  });
  app.mount(container);
}
