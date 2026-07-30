/**
 * 渲染端插件 SDK 后端（Tauri 版，移植自旧工程 desktop/src/renderer/plugin-sdk-browser.ts）。
 *
 * 与旧版的差异仅在宿主来源：旧版经 Electron preload 暴露 window.electronAPI，
 * 本版由适配层（src/api/index.ts，installHostApi）在 Tauri 环境下安装同形实现。
 * 插件业务代码（service/view）零改动。
 *
 * 插件 iframe 沙箱化（阶段 A 第三波）后，旧 tab 同进程初始化（initializePluginSDK，
 * 按窗口/URL query 解析域并写全局注入点）已随壳层旧注册路径一并退役；本模块只保留
 * createPluginBackend——桥 dispatcher（plugin-bridge-dispatcher.ts）以桥绑定的身份域
 * 构造后端，域一律显式下传给 plugin.* 命令。
 *
 * 类型（PluginSDK 等）的唯一来源是 @spark/plugin-sdk（code/packages/plugin-sdk，
 * 经相对路径引用），本模块 re-export 以保持既有 import 路径兼容。
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
// 后端构造（桥 dispatcher 使用）
// ------------------------------------------------------------------

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}

function resolveElectronAPI(): ElectronAPI | null {
  if (window.electronAPI) {
    return window.electronAPI;
  }

  // 同源 iframe 场景兜底：可经 parent 取宿主 API（对齐旧语义；
  // 沙箱 iframe 为 opaque origin，正常不会走到这里）
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

/**
 * 按域构造插件 SDK 后端。
 *
 * 桥模式：dispatcher 以桥绑定的身份域调用，主窗口无 tab URL 可回退，
 * 域一律显式传给 plugin.* 命令。
 *
 * @throws 如果宿主 API 不可用
 */
export function createPluginBackend(domain: string): PluginSDK {
  const electronAPI = resolveElectronAPI();
  if (!electronAPI) {
    throw new Error('electronAPI is not available in the renderer context');
  }
  const pluginDomain = domain;

  return {
    domain,
    evidence: electronAPI.evidence,
    p2p: electronAPI.p2p,
    runtime: {
      currentRoot: () => electronAPI.plugin.currentRoot(),
      syncOrganizationData: (orgId: string) =>
        electronAPI.plugin.syncOrganizationData(orgId, pluginDomain),
      listMineOrganizations: () =>
        electronAPI.plugin.listMineOrganizations(pluginDomain)
    },
    docs: {
      get: (collection: string, id: string) =>
        electronAPI.plugin.docGet(collection, id, pluginDomain),
      defineCollection: (collection: string, schema) =>
        electronAPI.plugin.docDeclareCollection(collection, schema, pluginDomain),
      put: (collection: string, id: string, doc: Record<string, unknown>) =>
        electronAPI.plugin.docPut(collection, id, doc, pluginDomain),
      delete: (collection: string, id: string) =>
        electronAPI.plugin.docDelete(collection, id, pluginDomain),
      query: (collection: string, options = {}) =>
        electronAPI.plugin.docQuery(collection, options, pluginDomain)
    },
    identity: {
      sign: (payload: string) =>
        electronAPI.plugin.identitySign(payload, pluginDomain),
      verify: (payload: string, signature: string, publicKey: string) =>
        electronAPI.plugin.identityVerify(payload, signature, publicKey)
    }
  };
}

