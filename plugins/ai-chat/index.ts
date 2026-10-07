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
import { registerBuiltinProviders, migrateLegacyApiKeys } from './service';

export async function bootstrapChat(): Promise<void> {
  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }

  // 先挂载 ChatView（initState='loading' 的加载页立即渲染，握手期间用户看到
  // "正在连接宿主环境…"而非白屏），桥握手在后台进行——成功后注入 SDK 触发
  // ensurePluginSDK 轮询命中，视图切正常态；失败则派 sdk-failed 事件切失败态。
  const app = createApp(ChatView);
  app.mount(container);

  try {
    const { sdk } = await connectPluginBridge({
      pluginId: manifestJson.id,
      viewId: 'chat',
      sdkVersion: manifestJson.sdkVersion,
    });
    // 注入 SDK 到全局注入点（模型、服务层均通过 ensurePluginSDK 读取）
    window.__sparkPluginSDK = sdk;
    registerBuiltinProviders();
    // 存量迁移（评审 H1 · R1.3）：ai_chat_bots 文档中的明文 apiKey 搬入
    // local scope 机密集合并从同步文档清除。幂等，失败不阻塞启动
    migrateLegacyApiKeys(sdk).catch((err) => {
      console.warn('[ai-chat] API Key 存量迁移失败:', err);
    });
  } catch (error) {
    console.error('[ai-chat] Bootstrap failed:', error);
    window.dispatchEvent(
      new CustomEvent('ai-chat:sdk-failed', {
        detail: error instanceof Error ? error.message : String(error),
      })
    );
  }
}

// 顶层自执行：插件入口即启动
bootstrapChat().catch((error) => {
  console.error('[ai-chat] Bootstrap failed:', error);
});
