/**
 * 最小聊天插件（spark-minichat）· 主入口（iframe 桥路径）。
 *
 * 验收样例（communication §六）：第三方最小聊天插件安装后读写同一消息
 * 数据——全部数据面只有 sdk.messages 四个调用（conversations/list/send/
 * onNewMessage），与默认内置 spark-chat 访问同一份 sled 消息数据。
 */
import { createApp } from 'vue';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import MiniChatApp from './src/MiniChatApp.vue';
import manifestJson from './manifest.json';
import { bindMiniChatSdk } from './src/sdk-access';

const manifest = manifestJson as PluginManifest;

// lib 构建产物须保留 ESM 导出（dist 自检口径；同时便于宿主/测试断言包身份）
export const PLUGIN_ID = manifest.id;

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
  bindMiniChatSdk(sdk, ctx);

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  createApp(MiniChatApp).mount(container);
}

if (isIframeBridgeContext()) {
  bootstrap().catch((error) => {
    console.error('[spark-minichat] iframe 桥初始化失败：', error);
  });
}
