/**
 * 聊天应用插件（spark-chat）· 主入口（iframe 桥路径，与 spark-moments 同构）。
 *
 * connectPluginBridge 握手拿 SDK 与运行上下文，写入 window.__sparkPluginSDK
 * 后自挂载到 #app。运行上下文（space）经 sdk-host 提供给 store（插件实例
 * 按空间绑定，space 切换由壳层重建实例）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ChatApp from './src/ChatApp.vue';
import manifestJson from './manifest.json';
import { bindPluginRuntime } from './src/sdk-host';

const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

async function bootstrap(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifest.id,
    viewId: manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;
  // store 数据面依赖的运行上下文（space 绑定 + SDK 句柄）
  bindPluginRuntime(sdk, ctx);

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(ChatApp);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-chat] iframe 桥初始化失败：', error);
  });
}
