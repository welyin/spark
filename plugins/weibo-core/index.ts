/**
 * weibo-core 插件入口（iframe 桥路径，插件 iframe 沙箱化阶段 A 第三波）。
 *
 * 插件只运行在沙箱 iframe 内（旧壳层编译期注册/tab 同进程路径已退役）：
 * 经 connectPluginBridge 握手拿 SDK 与运行上下文，写入全局注入点
 * window.__sparkPluginSDK 后自行挂载到宿主提供的 #app 容器——插件自挂载
 * 契约：宿主只负责握手与容器。definePlugin/PluginManifest 契约仍保留在
 * @spark/plugin-sdk（第三方插件约定），本插件不再走 definePlugin 导出。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import WeiboCoreView from './WeiboCoreView.vue';
import manifestJson from './manifest.json';

// JSON import 的类型是放宽后的结构（views.type 推为 string），此处收敛到 PluginManifest
const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/**
 * 握手 → SDK 写全局注入点（视图组件经 ensurePluginSDK 读取，业务组件零改动）
 * → 挂载 #app（ElementPlus 在 mount 内自行 use）。
 */
async function bootstrapIframe(): Promise<void> {
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
  const app = createApp(WeiboCoreView);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrapIframe().catch((error) => {
    console.error('[weibo-core] iframe 桥初始化失败：', error);
  });
}
