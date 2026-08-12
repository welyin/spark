/**
 * 朋友圈插件（spark-moments）· 主入口（iframe 桥路径）。
 *
 * 与 spark-example 同构：connectPluginBridge 握手拿 SDK 与运行上下文，写入
 * window.__sparkPluginSDK 后自挂载到 #app。多视图分发：宿主 srcdoc 固定加载
 * views/main.js，按 window.__sparkPluginView 分发——message-card 上下文加载
 * 卡片视图（notify-card），否则挂载主视图（MomentsView）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import MomentsView from './MomentsView.vue';
import manifestJson from './manifest.json';

// JSON import 类型放宽，收敛到 PluginManifest
const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/** 主视图（app）引导：握手 → SDK 写全局注入点 → 挂载 #app */
async function bootstrapMainView(): Promise<void> {
  const { sdk } = await connectPluginBridge({
    pluginId: manifest.id,
    viewId: manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(MomentsView);
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapNotifyCard } = await import('./notify-card');
  await bootstrapNotifyCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-moments] iframe 桥初始化失败：', error);
  });
}
