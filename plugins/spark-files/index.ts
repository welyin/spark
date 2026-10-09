/**
 * 文件管理插件（spark-files）· 主入口（iframe 桥路径，与 spark-market 同构）。
 *
 * connectPluginBridge 握手拿 SDK 与运行上下文，写入 window.__sparkPluginSDK
 * 后自挂载到 #app。个人空间与组织域同一界面：空间边界由桥按绑定空间注入
 * （sdk.data 在 org 空间自动落组织集合），各走各的域，插件不判断空间。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import FilesApp from './src/FilesApp.vue';
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
    // viewId 缺省回退宿主 srcdoc 注入（hello 身份核对要求与桥绑定一致）
    viewId: window.__sparkPluginView?.viewId ?? manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;
  bindPluginRuntime(sdk, ctx);

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(FilesApp);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-files] iframe 桥初始化失败：', error);
  });
}
