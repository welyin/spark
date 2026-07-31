/**
 * 示例插件（spark-example）· message-card 视图入口（帖子卡片）。
 *
 * 与主视图入口（index.ts）同构：connectPluginBridge 握手拿 SDK 与运行上下文
 * （viewId 固定 'post-card'，须与 manifest.views 声明一致），SDK 写入全局
 * 注入点后挂载 PostCard 到宿主 #app 容器。
 *
 * 差异：卡片上下文（ctx.mount）携带 cardId 与 cardData——cardData 即应用
 * 消息的 card.data 透传，经 props 交给视图组件；triggerCardAction 所需的
 * cardId 由桥客户端从 ctx 自取，视图无需经手。
 *
 * 入口形态（诚实口径）：当前壳层固定加载 views/main.js，由主入口按
 * window.__sparkPluginView 内部分发到本模块——这是现实约束下的 workaround，
 * 并非设计终态；设计方向是壳层按 view 直载 views/<viewId>.js（设计文档
 * 「包形态」多视图约定）。本模块作为独立 vite 入口产出
 * dist/views/post-card.js，即为按 view 直载预留的产物：届时壳层直接加载它，
 * 无需经主入口转发。因此只导出引导函数、不在顶层自执行。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import PostCard from './PostCard.vue';
import manifestJson from './manifest.json';

export async function bootstrapPostCard(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifestJson.id,
    viewId: 'post-card',
    sdkVersion: manifestJson.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(PostCard, { cardData: ctx.mount.cardData as { postId?: string } | undefined });
  app.mount(container);
}
