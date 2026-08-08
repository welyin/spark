/**
 * ai-chat 插件 · 入口（chat 视图）。
 *
 * 引导流程：
 * 1. connectPluginBridge 握手拿 SDK 与运行上下文
 * 2. SDK 写入全局注入点 window.__sparkPluginSDK
 * 3. 后端提供者（CodeBuddy CLI / OpenAI / Ollama）已由 ChatView.vue 模块顶层注册
 * 4. 挂载 ChatView 到宿主 #app 容器
 *
 * 注意：当前 SDK 版本以 views/main.js 作为默认入口，由壳层固定加载。
 * 设计方向是壳层按 view 直载 views/<viewId>.js，本模块产出
 * dist/views/chat.js 即为按 view 直载预留的产物：届时壳层直接加载它。
 */
import { createApp } from 'vue';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ChatView from './ChatView.vue';
import manifestJson from './manifest.json';
import { startBackgroundBotListeners, registerBuiltinProviders } from './service';

export async function bootstrapChat(): Promise<void> {
  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }

  // 先握手拿到运行上下文：需要 ctx.mount.viewType 区分本实例是
  // 主视图（app）还是后台常驻视图（background）。
  let sdk;
  let ctx;
  // viewId 用宿主注入的 srcdoc 变量（window.__sparkPluginView.viewId），
  // 不能写死——后台视图与主视图共用 main.js，硬编码 'chat' 会让后台视图
  // 握手时 identity mismatch（宿主期望 ai-chat/background，插件报 ai-chat/chat）
  const viewId =
    (window as unknown as { __sparkPluginView?: { viewId?: string } }).__sparkPluginView?.viewId ?? 'chat';
  try {
    const connected = await connectPluginBridge({
      pluginId: manifestJson.id,
      viewId,
      sdkVersion: manifestJson.sdkVersion,
    });
    sdk = connected.sdk;
    ctx = connected.ctx;
    // 注入 SDK 到全局注入点（模型、服务层均通过 ensurePluginSDK 读取）
    window.__sparkPluginSDK = sdk;
    // 注册内置后端提供者（codebuddy/openai/ollama）——必须在分流前调用，
    // 后台视图（background）不挂载 ChatView，provider 注册收归入口统一执行，
    // 否则后台监听调 handleMainChatMessage 会报"未注册的后端类型"
    registerBuiltinProviders();
  } catch (error) {
    console.error('[ai-chat] Bootstrap failed:', error);
    // 握手失败：主视图挂载错误态 UI，后台视图静默退出
    const app = createApp(ChatView);
    app.mount(container);
    window.dispatchEvent(
      new CustomEvent('ai-chat:sdk-failed', {
        detail: error instanceof Error ? error.message : String(error),
      })
    );
    return;
  }

  // 后台常驻视图：只启动 bot 消息监听，不挂载 UI（隐藏 iframe，无界面）
  if (ctx.mount.viewType === 'background') {
    await startBackgroundBotListeners(sdk);
    return;
  }

  // 主视图（app）：挂载聊天 UI。SDK 已注入，onMounted 的 ensurePluginSDK 立即可得
  const app = createApp(ChatView);
  app.mount(container);
}

// 顶层自执行：插件入口即启动
bootstrapChat().catch((error) => {
  console.error('[ai-chat] Bootstrap failed:', error);
});
