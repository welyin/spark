/**
 * 担保链门槛示例（spark-threshold-vouch）· 主入口（iframe 桥路径）。
 *
 * 结构对齐 spark-example/index.ts：connectPluginBridge 握手 → SDK 写全局注入点
 * window.__sparkPluginSDK → 自挂载 #app。
 *
 * 能力面（community-affairs.md §7.3 门槛插件）：docs（请求/担保/证明三个
 * append-only 集合）+ identity:sign（担保与组装签名——签名主体为插件域
 * 身份，不证明担保人/组装人个人身份；诚实口径见 service.ts 头注与
 * README.md）。
 * 证明产物由 identity.verify 免权限验签——验的是插件域签名与载荷完整性。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import VouchView from './VouchView.vue';
import manifestJson from './manifest.json';

const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

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
  const app = createApp(VouchView);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrapMainView().catch((error) => {
    console.error('[spark-threshold-vouch] iframe 桥初始化失败：', error);
  });
}
