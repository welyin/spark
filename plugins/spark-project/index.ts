/**
 * 项目（议题）插件（spark-project）· 主入口（iframe 桥路径）。
 *
 * 复用插件体系统一入口结构（spark-kanban/spark-forum 同构）：插件只运行在
 * 沙箱 iframe 内，经 connectPluginBridge 握手拿 SDK 与运行上下文，写入全局
 * 注入点 window.__sparkPluginSDK 后自行挂载到宿主提供的 #app 容器。
 *
 * 多视图分发：宿主 srcdoc 固定加载 views/main.js，入口按宿主注入的
 * window.__sparkPluginView 分发——message-card 上下文按 viewId 分流到
 * 项目动态卡片（affair-card）/ 发布卡片（release-card，组合 release-manager
 * 库件的推送渲染面），否则挂载主工作区视图。
 *
 * 空间上下文注入：桥握手 ctx.space 经 props 传给主视图——个人空间固定
 * 'personal' 数据域（发布管理件仅组织空间，个人空间该区显示「未启用」），
 * 组织空间走组织选择器。
 *
 * 移动端只读（档三-2）：ctx.platform 为 android/ios 时视图层禁用全部写
 * 入口并如实标注（壳层 enforcement 线形未落地，插件自查，同 git-repo）。
 */
import { createApp } from 'vue';
import ElementPlus from 'element-plus';
// 框架自包含：iframe 形态下 ElementPlus 样式随 bundle 打进 assets/main.css
import 'element-plus/dist/index.css';
import type { PluginManifest } from '../../packages/plugin-sdk/src';
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import ProjectView from './ProjectView.vue';
import manifestJson from './manifest.json';

// JSON import 的类型是放宽后的结构（views.type 推为 string），此处收敛到 PluginManifest
const manifest = manifestJson as PluginManifest;

/** 插件 id（具名导出：dist 自检要求视图 bundle 含 ESM 导出语法，亦供类型注册表核对） */
export const SPARK_PROJECT_PLUGIN_ID = manifest.id;

/** iframe 桥上下文判定：非顶层窗口且尚无注入点（插件 bundle 只在沙箱 iframe 内加载） */
function isIframeBridgeContext(): boolean {
  return window.parent !== window && !window.__sparkPluginSDK;
}

/** 主视图（app）引导：握手 → SDK 写全局注入点 → 挂载 #app（空间/平台上下文经 props 注入） */
async function bootstrapMainView(): Promise<void> {
  const { sdk, ctx } = await connectPluginBridge({
    pluginId: manifest.id,
    viewId: manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;

  const container = document.getElementById('app');
  if (!container) {
    throw new Error('plugin host container #app not found');
  }
  const app = createApp(ProjectView, {
    pluginContext: {
      spaceType: ctx.space.type,
      orgId: ctx.space.type === 'org' ? ctx.space.id : undefined,
      ...(ctx.platform ? { platform: ctx.platform } : {})
    }
  });
  app.use(ElementPlus);
  app.mount(container);
}

/** 卡片视图（message-card）引导：按 viewId 分流，独立 bundle 避免拖累消息流 */
async function bootstrapCardView(viewId: string): Promise<void> {
  if (viewId === 'release-card') {
    const { bootstrapReleaseCard } = await import('./release-card');
    await bootstrapReleaseCard();
    return;
  }
  const { bootstrapProjectCard } = await import('./affair-card');
  await bootstrapProjectCard();
}

if (isIframeBridgeContext()) {
  const view = window.__sparkPluginView;
  const bootstrap = view?.viewType === 'message-card' ? bootstrapCardView(view.viewId) : bootstrapMainView();
  bootstrap.catch((error) => {
    console.error('[spark-project] iframe 桥初始化失败：', error);
  });
}
