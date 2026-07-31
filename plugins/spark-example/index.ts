/**
 * 示例插件（spark-example）· 主入口（iframe 桥路径，插件 iframe 沙箱化阶段 A 第三波）。
 *
 * 插件只运行在沙箱 iframe 内（旧壳层编译期注册/tab 同进程路径已退役）：
 * 经 connectPluginBridge 握手拿 SDK 与运行上下文，写入全局注入点
 * window.__sparkPluginSDK 后自行挂载到宿主提供的 #app 容器——插件自挂载
 * 契约：宿主只负责握手与容器。definePlugin/PluginManifest 契约仍保留在
 * @spark/plugin-sdk（第三方插件约定），本插件不再走 definePlugin 导出。
 *
 * 多视图分发（教学要点）：宿主 srcdoc 固定加载 views/main.js（见壳层
 * plugin-source.ts），所有视图共用这一个入口；入口按宿主注入的
 * window.__sparkPluginView 分发——message-card 上下文动态加载卡片视图
 * （独立 bundle dist/views/post-card.js，设计文档「包形态」多视图约定），
 * 否则挂载主视图。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ExampleView from './ExampleView.vue';
import manifestJson from './manifest.json';

// JSON import 的类型是放宽后的结构（views.type 推为 string），此处收敛到 PluginManifest
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
  const app = createApp(ExampleView);
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapPostCard } = await import('./post-card');
  await bootstrapPostCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-example] iframe 桥初始化失败：', error);
  });
}
