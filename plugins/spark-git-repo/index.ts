/**
 * 代码仓库应用（spark-git-repo）· 主入口（iframe 桥路径）。
 *
 * 结构对齐 spark-affairs/index.ts：connectPluginBridge 握手 → SDK 写全局注入点
 * window.__sparkPluginSDK → 自挂载 #app；message-card 上下文动态加载卡片 bundle。
 *
 * 能力面（git-repo.md v0.2）：sdk.affairs（镜像清单 = 议题内签名事务操作，档二-5；
 * PR = 子事务 + pr.* 操作集）+ sdk.content（逐 Git 对象散 blob / bundle 附件，
 * 档三-6）+ sdk.sys（桌面 git CLI，system:exec 高危权限）+ message:app（PR 卡片）。
 * 移动端只读降级：写入口隐藏（mobileReadonly 口径），浏览纯 JS 不经 CLI。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import RepoView from './RepoView.vue';
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
  const app = createApp(RepoView);
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按需加载独立 bundle，避免主视图体积拖累消息流 */
async function bootstrapCardView(): Promise<void> {
  const { bootstrapPrCard } = await import('./pr-card');
  await bootstrapPrCard();
}

if (isIframeBridgeContext()) {
  const bootstrap =
    window.__sparkPluginView?.viewType === 'message-card' ? bootstrapCardView() : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-git-repo] iframe 桥初始化失败：', error);
  });
}
