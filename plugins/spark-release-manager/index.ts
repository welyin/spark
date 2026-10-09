/**
 * 发布管理插件（spark-release-manager）· 主入口（iframe 桥路径）。
 *
 * 直接复用 spark-kanban 的入口结构：插件只运行在沙箱 iframe 内，经
 * connectPluginBridge 握手拿 SDK 与运行上下文，写入全局注入点
 * window.__sparkPluginSDK 后自行挂载到宿主提供的 #app 容器。
 *
 * 多视图分发：宿主 srcdoc 固定加载 views/main.js，入口按宿主注入的
 * window.__sparkPluginView 分发——message-card 上下文动态加载卡片视图
 * （独立 bundle dist/views/release-card.js），否则挂载主视图。
 *
 * 空间上下文注入：桥握手 ctx.space 经 props 传给主视图（supportedSpaces
 * 仅 org——发布是组织域业务）；握手 ctx.platform 一并注入（档二-8：
 * 移动端降级只做登记，视图层据此隐藏核验入口）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ReleaseManagerView from './ReleaseManagerView.vue';
import manifestJson from './manifest.json';

// JSON import 的类型是放宽后的结构（views.type 推为 string），此处收敛到 PluginManifest
const manifest = manifestJson as PluginManifest;

/** 插件 id（具名导出：dist 自检要求视图 bundle 含 ESM 导出语法，亦供库消费方核对） */
export const SPARK_RELEASE_MANAGER_PLUGIN_ID = manifest.id;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/** 主视图（app）引导：握手 → SDK 写全局注入点 → 挂载 #app（空间/平台上下文经 props 注入） */
async function bootstrapMainView(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifest.id,
    viewId: manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(ReleaseManagerView, {
    pluginContext: {
      spaceType: ctx.space.type,
      orgId: ctx.space.type === 'org' ? ctx.space.id : undefined,
      platform: ctx.platform
    }
  });
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapReleaseCard } = await import('./release-card');
  await bootstrapReleaseCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-release-manager] iframe 桥初始化失败：', error);
  });
}
