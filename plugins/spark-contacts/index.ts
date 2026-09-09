/**
 * 通讯录应用插件（spark-contacts）· 主入口（iframe 桥路径，与 spark-chat 同构）。
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
import ContactsApp from './src/ContactsApp.vue';
import manifestJson from './manifest.json';
import { bindPluginRuntime, currentRoot } from './src/sdk-host';
import { refreshCurrentUser } from './src/current-user';
import { setSelfProfileExtra } from './src/profile-extra';
import { refreshOrganizations } from './src/org-membership';

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

  // 当前身份水合（头像/展示名/资料扩展字段）；失败静默（展示走自动头像兜底）
  void refreshCurrentUser().then(async () => {
    try {
      const status = await currentRoot();
      if (status.rootId) {
        setSelfProfileExtra(status.rootId, status);
      }
    } catch {
      // 扩展字段缺失仅影响签名/性别展示，不阻断
    }
  });
  // 组织成员缓存预拉（组织空间通讯录列表数据源；个人空间为空列表，无害）
  void refreshOrganizations().catch(() => {});

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(ContactsApp);
  app.use(ElementPlus);
  app.mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-contacts] iframe 桥初始化失败：', error);
  });
}
