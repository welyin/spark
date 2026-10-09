/**
 * 问题反馈应用（spark-feedback）· 主入口（iframe 桥路径）。
 *
 * 结构对齐 spark-git-repo/index.ts：connectPluginBridge 握手 → SDK 写全局注入点
 * window.__sparkPluginSDK → 自挂载 #app；message-card 上下文动态加载卡片 bundle。
 * 桥握手 ctx（appVersion/platform/shellVersion，档二-9）经 props 传入主视图，
 * 供环境信息附带与降级判定。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import FeedbackView from './FeedbackView.vue';
import manifestJson from './manifest.json';

const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/** 主视图（app）引导：握手 → SDK 写全局注入点 → 挂载 #app（ctx 下行供环境信息附带） */
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
  const app = createApp(FeedbackView, { ctx });
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapFeedbackCard } = await import('./feedback-card');
  await bootstrapFeedbackCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-feedback] iframe 桥初始化失败：', error);
  });
}
