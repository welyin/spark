/**
 * 渲染端插件 SDK（Tauri 版，移植自旧工程 desktop/src/renderer/plugin-sdk-browser.ts）。
 *
 * 与旧版的差异仅在宿主来源：旧版经 Electron preload 暴露 window.electronAPI，
 * 本版由适配层（src/api/index.ts，installHostApi）在 Tauri 环境下安装同形实现。
 * 初始化流程与域解析语义与旧版完全一致，插件业务代码（service/view）零改动。
 *
 * 类型（PluginSDK 等）已迁到独立包 @spark/plugin-sdk（code/packages/plugin-sdk，
 * 经相对路径引用），本模块 re-export 以保持既有 import 路径兼容，并持有
 * Tauri 实现；initializePluginSDK 完成时把实例写入全局注入点
 * window.__sparkPluginSDK，供插件侧经 @spark/plugin-sdk 的
 * getPluginSDK/ensurePluginSDK 读取。
 */

import type { ElectronAPI } from './api';
import type { PluginSDK } from '../../packages/plugin-sdk/src';

// 类型门面：SDK 类型的唯一来源是 @spark/plugin-sdk，此处统一 re-export
export type {
  PluginQueryFilter,
  PluginDocQueryOptions,
  PluginEvidenceAPI,
  PluginP2PAPI,
  PluginRuntimeAPI,
  PluginDocAPI,
  PluginIdentityAPI,
  PluginCollectionSchema,
  PluginDeclaredCollectionSchema,
  PluginSDK
} from '../../packages/plugin-sdk/src';

// ------------------------------------------------------------------
// 初始化（逻辑与旧 plugin-sdk-browser.ts 一致）
// ------------------------------------------------------------------

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}

let cachedSDK: PluginSDK | null = null;

function resolveElectronAPI(): ElectronAPI | null {
  if (window.electronAPI) {
    return window.electronAPI;
  }

  // iframe 场景兜底：插件 tab 与主窗口同源，可经 parent 取宿主 API。
  // （Tauri 下 installHostApi 会在每次页面加载时安装 window.electronAPI，
  // iframe 内通常直接命中上面的分支；保留 parent 回退对齐旧语义。）
  try {
    const parentApi = (window.parent as Window & { electronAPI?: ElectronAPI } | null)?.electronAPI;
    if (parentApi) {
      return parentApi;
    }
  } catch {
    // Ignore cross-frame access errors and fall through.
  }

  return null;
}

function resolveRequestedPluginDomain(): string | null {
  const search = new URLSearchParams(window.location.search);
  const fromQuery = search.get('pluginDomain')?.trim() ?? '';
  if (!fromQuery) {
    return null;
  }
  if (!fromQuery.startsWith('plugin:') || fromQuery.length <= 'plugin:'.length) {
    return null;
  }
  return fromQuery;
}

/**
 * 初始化插件 SDK
 *
 * 安全说明（本期沿用旧 tab 模式语义）：插件运行在 system 域窗口的 iframe tab
 * 内，域身份由 URL query `pluginDomain` 显式给定；独立插件窗口由宿主绑定域、
 * 渲染端不可指定，待插件运行时排期。
 *
 * @throws 如果宿主 API 不可用，或当前窗口解析不出合法插件域
 */
export async function initializePluginSDK(): Promise<PluginSDK> {
  const electronAPI = resolveElectronAPI();
  if (!electronAPI) {
    throw new Error('electronAPI is not available in the renderer context');
  }

  const result = await electronAPI.getDomain();
  const currentDomain = result?.domain;
  const requestedDomain = resolveRequestedPluginDomain();
  const domain =
    currentDomain && currentDomain.startsWith('plugin:') && currentDomain.length > 'plugin:'.length
      ? currentDomain
      : requestedDomain;

  if (!domain || !domain.startsWith('plugin:')) {
    throw new Error(
      `Plugin SDK initialization failed: current window domain is "${currentDomain}". ` +
      'Plugin windows must be created with a plugin: domain by the main process.'
    );
  }

  const needsExplicitPluginDomain = !(currentDomain && currentDomain === domain);

  cachedSDK = {
    domain,
    evidence: electronAPI.evidence,
    p2p: electronAPI.p2p,
    runtime: {
      currentRoot: () => electronAPI.plugin.currentRoot(),
      syncOrganizationData: (orgId: string) =>
        electronAPI.plugin.syncOrganizationData(orgId, needsExplicitPluginDomain ? domain : undefined),
      listMineOrganizations: () =>
        electronAPI.plugin.listMineOrganizations(needsExplicitPluginDomain ? domain : undefined)
    },
    docs: {
      get: (collection: string, id: string) =>
        electronAPI.plugin.docGet(collection, id, needsExplicitPluginDomain ? domain : undefined),
      defineCollection: (collection: string, schema) =>
        electronAPI.plugin.docDeclareCollection(collection, schema, needsExplicitPluginDomain ? domain : undefined),
      put: (collection: string, id: string, doc: Record<string, unknown>) =>
        electronAPI.plugin.docPut(collection, id, doc, needsExplicitPluginDomain ? domain : undefined),
      delete: (collection: string, id: string) =>
        electronAPI.plugin.docDelete(collection, id, needsExplicitPluginDomain ? domain : undefined),
      query: (collection: string, options = {}) =>
        electronAPI.plugin.docQuery(collection, options, needsExplicitPluginDomain ? domain : undefined)
    },
    identity: {
      sign: (payload: string) =>
        electronAPI.plugin.identitySign(payload, needsExplicitPluginDomain ? domain : undefined),
      verify: (payload: string, signature: string, publicKey: string) =>
        electronAPI.plugin.identityVerify(payload, signature, publicKey)
    }
  };

  // 写入全局注入点：插件侧（@spark/plugin-sdk 的 getPluginSDK/ensurePluginSDK）从这里读取
  window.__sparkPluginSDK = cachedSDK;

  return cachedSDK;
}

/**
 * 获取已初始化的插件 SDK 实例
 *
 * @throws 如果尚未调用 initializePluginSDK
 */
export function getPluginSDK(): PluginSDK {
  if (!cachedSDK) {
    throw new Error('Plugin SDK is not initialized. Call initializePluginSDK() first.');
  }
  return cachedSDK;
}
