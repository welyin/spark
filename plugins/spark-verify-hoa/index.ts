/**
 * 业主资格验证示例（spark-verify-hoa）· 主入口（iframe 桥路径）。
 *
 * 结构对齐 spark-example/index.ts：connectPluginBridge 握手 → SDK 写全局注入点
 * window.__sparkPluginSDK → 自挂载 #app。
 *
 * 强制 L1 开源语义（community-model.md §十）：验证插件经手个人敏感材料，
 * 强制源码公开可审计。manifest.json 由宿主 fetch + JSON.parse 加载（严格
 * JSON，不允许注释），故 L1 声明双写在 manifest.description 与
 * spark-plugin.json 的 comment 字段；L1 级别的实际校验在分发/市场侧，
 * 插件自身无法自证。
 *
 * 能力面：sdk.credentials 只读三方法（listHeld / presentHolderProof /
 * queryVerifiers，credentials:read，类型对接见 sdk-credentials.ts）+
 * identity:sign（签发/注销签名——签名主体为插件域身份，产物为演示级
 * 线形，诚实口径见 service.ts 头注与 README.md）+ docs（申请/凭证/注销
 * 记录，append-only）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import HoaVerifyView from './HoaVerifyView.vue';
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
  const app = createApp(HoaVerifyView);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrapMainView().catch((error) => {
    console.error('[spark-verify-hoa] iframe 桥初始化失败：', error);
  });
}
