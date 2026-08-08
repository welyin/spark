/**
 * ai-chat 插件 · 入口（chat 视图，纯 UI）。
 *
 * 引导流程：
 * 1. connectPluginBridge 握手拿 SDK
 * 2. SDK 写入全局注入点 window.__sparkPluginSDK
 * 3. 注册内置后端提供者（插件内聊天用）
 * 4. 挂载 ChatView 到宿主 #app 容器
 *
 * bot 联系人的消息监听在后台入口（background.ts，内核 QuickJS 沙箱），
 * 与本视图无关——主界面开不开，bot 都在线。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ChatView from './ChatView.vue';
import manifestJson from './manifest.json';
import { registerBuiltinProviders } from './service';

export async function bootstrapChat(): Promise<void> {
  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }

  try {
    const { sdk } = await connectPluginBridge({
      pluginId: manifestJson.id,
      viewId: 'chat',
      sdkVersion: manifestJson.sdkVersion,
    });
    // 注入 SDK 到全局注入点（模型、服务层均通过 ensurePluginSDK 读取）
    window.__sparkPluginSDK = sdk;
    registerBuiltinProviders();
  } catch (error) {
    console.error('[ai-chat] Bootstrap failed:', error);
    const app = createApp(ChatView);
    app.mount(container);
    window.dispatchEvent(
      new CustomEvent('ai-chat:sdk-failed', {
        detail: error instanceof Error ? error.message : String(error),
      })
    );
    return;
  }

  const app = createApp(ChatView);
  app.mount(container);
}

// 顶层自执行：插件入口即启动
bootstrapChat().catch((error) => {
  console.error('[ai-chat] Bootstrap failed:', error);
});
