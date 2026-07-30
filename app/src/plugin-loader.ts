/**
 * 插件视图自动加载器。
 *
 * 通过 Vite 的 import.meta.glob 扫描 code/plugins 下的插件入口，
 * 每个插件入口默认导出 definePlugin 的返回值（@spark/plugin-sdk 入口契约）；
 * 本加载器校验 manifest 后调用 setup(ctx)，ctx.registerView 桥接到
 * plugin-view-registry 完成注册。内核（App.vue / main.ts）无需感知具体插件。
 *
 * 插件目录在 src 之外（code/plugins，与 code/app 平级）：
 * - dev：依赖 vite.config.ts 的 server.fs.allow 放开上级目录；
 * - build：import.meta.glob 由 Vite 编译期展开，不受 fs.allow 限制。
 */
import type { Component } from 'vue';
import type { PluginDefinition, PluginSDK } from '../../packages/plugin-sdk/src';
import { registerPluginView } from './plugin-view-registry';
import { initializePluginSDK } from './plugin-sdk-browser';

const pluginEntries = import.meta.glob<{ default?: PluginDefinition }>('../../plugins/*/index.ts', { eager: true });

/** manifest 基础校验：域前缀合法、entryView 对应的视图已声明 */
function validateManifest(def: PluginDefinition): string | null {
  const { manifest } = def;
  if (!manifest || typeof manifest !== 'object') {
    return '缺少 manifest';
  }
  if (!manifest.domain || !manifest.domain.startsWith('plugin:') || manifest.domain.length <= 'plugin:'.length) {
    return `非法插件域：${manifest.domain}`;
  }
  if (!Array.isArray(manifest.views) || manifest.views.length === 0) {
    return 'manifest.views 为空';
  }
  if (!manifest.views.some((view) => view.id === manifest.entryView)) {
    return `entryView "${manifest.entryView}" 未在 views 中声明`;
  }
  return null;
}

/**
 * ctx.sdk：插件 SDK 在 tab 模式下按窗口异步注入（window.__sparkPluginSDK），
 * setup 执行时通常尚未就绪；这里给惰性代理，真正调用时才读取注入点，
 * 未注入时报明确的错误（当前 weibo-core 的 setup 只用 registerView）。
 */
function createLazyPluginSdk(): PluginSDK {
  return new Proxy({} as PluginSDK, {
    get(_, prop) {
      const sdk = window.__sparkPluginSDK;
      if (!sdk) {
        throw new Error('Plugin SDK is not injected yet (available only in plugin tab context).');
      }
      return sdk[prop as keyof PluginSDK];
    }
  });
}

/** 当前窗口是否为插件 tab 上下文（URL query 带 pluginDomain） */
function isPluginTabContext(): boolean {
  const domain = new URLSearchParams(window.location.search).get('pluginDomain')?.trim() ?? '';
  return domain.startsWith('plugin:') && domain.length > 'plugin:'.length;
}

export function initializePlugins(): void {
  for (const [entryPath, module] of Object.entries(pluginEntries)) {
    // 单个插件异常不应拖垮其他插件加载
    try {
      const def = module.default;
      if (!def || typeof def.setup !== 'function') {
        console.error(`[plugin-loader] 插件入口缺少 definePlugin 默认导出：${entryPath}`);
        continue;
      }
      const invalidReason = validateManifest(def);
      if (invalidReason) {
        console.error(`[plugin-loader] 插件 manifest 校验失败（${entryPath}）：${invalidReason}`);
        continue;
      }

      def.setup({
        sdk: createLazyPluginSdk(),
        registerView: (viewId, component) =>
          registerPluginView(def.manifest.domain, viewId, component as Component)
      });
    } catch (error) {
      console.error(`[plugin-loader] 插件加载失败（${entryPath}）：`, error);
    }
  }

  // 插件 tab 上下文：预热 SDK 并写入全局注入点（initializePluginSDK 内完成），
  // 插件视图经 @spark/plugin-sdk 的 ensurePluginSDK 挂起等待注入
  if (isPluginTabContext()) {
    initializePluginSDK().catch((error) => {
      console.error('[plugin-loader] 插件 SDK 初始化失败：', error);
    });
  }
}
