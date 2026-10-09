/**
 * 组织管理插件（spark-org-admin）· 主入口（iframe 桥路径，与 spark-market 同构）。
 *
 * connectPluginBridge 握手拿 SDK 与运行上下文，写入 window.__sparkPluginSDK
 * 后自挂载到 #app。空间语义：org 空间 = 当前空间组织的管理界面（信息/名册/
 * 策略/公开/发现/找回/数据治理）；personal 空间 = 创建/加入 + 如实提示
 * （listMine personal 拒绝——A42 评审决议，见 plugin-sdk sdk.org 头注）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import OrgAdminApp from './src/OrgAdminApp.vue';
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
  const app = createApp(OrgAdminApp);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-org-admin] iframe 桥初始化失败：', error);
  });
}
