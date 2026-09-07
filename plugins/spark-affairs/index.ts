/**
 * 公共议题客户端（spark-affairs）· 主入口（iframe 桥路径）。
 *
 * 结构对齐 spark-example/index.ts：connectPluginBridge 握手 → SDK 写全局注入点
 * window.__sparkPluginSDK → 自挂载 #app；多视图按 window.__sparkPluginView
 * 分发——message-card 上下文动态加载卡片 bundle（dist/views/affair-card.js），
 * 否则挂载主视图 AffairsView。
 *
 * 能力面对齐 wiki/architecture/community-affairs.md §7.2/§9 C11 已落地 SDK 面：
 * sdk.affairs（create（SDK 承载创世构造+签名+follow，refs 走 §10 类型化暴露）/
 * follow(genesis) / unfollow / listFollowed / submitOp / readLog /
 * readResolution / ladderStatus / onChange 变更订阅）+ identity:sign（操作
 * 协议签名，插件域身份）+ message:app（本机应用会话通知 + 议题卡片）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import AffairsView from './AffairsView.vue';
import manifestJson from './manifest.json';

const manifest = manifestJson as PluginManifest;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/** 主视图（app）引导：握手 → SDK 写全局注入点 → 挂载 #app */
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
  const app = createApp(AffairsView);
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapAffairCard } = await import('./affair-card');
  await bootstrapAffairCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-affairs] iframe 桥初始化失败：', error);
  });
}
