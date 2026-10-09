/**
 * 项目（议题）插件（spark-project）· message-card 视图入口（项目动态卡片）。
 *
 * 与主视图入口（index.ts）同构：connectPluginBridge 握手拿 SDK 与运行上下文
 * （viewId 固定 'affair-card'，须与 manifest.views 声明一致），SDK 写入全局
 * 注入点后挂载 ProjectCard 到宿主 #app 容器。卡片 data 只携带引用
 * { kind, affairId, title, action? }——议题内容经 sdk.affairs 重读，不随消息
 * 冗余落库；未装插件的成员看到纯文本摘要（消息自描述约定，summary 必填）。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ProjectCard from './ProjectCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapProjectCard(): Promise<void> {
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
  const app = createApp(ProjectCard, {
    cardData: ctx.mount.cardData as
      | { kind?: 'project-created' | 'child-disposition'; affairId?: string; title?: string; action?: string }
      | undefined
  });
  app.mount(container);
}
