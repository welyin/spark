/**
 * 应用市场插件（spark-market）· 主入口（iframe 桥路径，与 spark-chat 同构）。
 *
 * connectPluginBridge 握手拿 SDK 与运行上下文，写入 window.__sparkPluginSDK
 * 后自挂载到 #app。多视图分发（与 spark-moments 同口径）：viewId 以宿主
 * srcdoc 注入（window.__sparkPluginView）为准——壳层 BuiltinAppHost 的
 * initial-view 直达（如 Dock「应用市场」→ market 页）即经此传递；
 * 缺省回退 manifest.entryView。起步子视图由 MarketApp 按 ctx.viewId 决定。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import MarketApp from './src/MarketApp.vue';
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
    // viewId 缺省回退宿主 srcdoc 注入（hello 身份核对要求与桥绑定一致；
    // 显式传入宿主注入值，只在无注入的异常场景回退 entryView）
    viewId: window.__sparkPluginView?.viewId ?? manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;
  bindPluginRuntime(sdk, ctx);

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(MarketApp);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-market] iframe 桥初始化失败：', error);
  });
}
