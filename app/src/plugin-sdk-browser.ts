/**
 * 渲染端插件 SDK（Tauri 版，移植自旧工程 desktop/src/renderer/plugin-sdk-browser.ts）。
 *
 * 与旧版的差异仅在宿主来源：旧版经 Electron preload 暴露 window.electronAPI，
 * 本版由适配层（src/api/index.ts，installHostApi）在 Tauri 环境下安装同形实现。
 * 初始化流程与域解析语义与旧版完全一致，插件业务代码（service/view）零改动。
 *
 * 类型（PluginSDK 等）内联自旧工程 desktop/src/main/plugins/sdk.ts，
 * 使本模块成为插件侧唯一的 SDK 引入口。
 */

import type { DomainSignature, ElectronAPI } from './api';

// ------------------------------------------------------------------
// SDK 类型（内联自旧 desktop/src/main/plugins/sdk.ts）
// ------------------------------------------------------------------

export type PluginQueryFilter = {
  field: string;
  value: string | number | boolean;
  op?: 'eq' | 'startsWith' | 'gt' | 'lt' | 'gte' | 'lte';
};

export type PluginDocQueryOptions = {
  limit?: number;
  reverse?: boolean;
  filter?: PluginQueryFilter[];
};

export interface PluginEvidenceAPI {
  headHash: () => Promise<{ hash: string | null }>;
  verify: () => Promise<{ valid: boolean; height: number }>;
}

export interface PluginP2PAPI {
  start: () => Promise<{ started: boolean }>;
  stop: () => Promise<{ started: boolean }>;
  broadcast: (topic: string, message: Record<string, any>) => Promise<{ success: boolean }>;
}

export interface PluginRuntimeAPI {
  currentRoot: () => Promise<{ unlocked: boolean; rootId: string | null }>;
  syncOrganizationData: (orgId: string) => Promise<{ orgId: string; attempted: number; pulled: number }>;
  listMineOrganizations: () => Promise<Array<{
    orgId: string;
    name: string;
    description: string;
    basePluginDomain?: string;
    currentUserRole: 'admin' | 'member' | null;
    isCurrentUserAdmin: boolean;
    memberCount: number;
    adminCount: number;
    members: Array<{
      rootId: string;
      role: 'admin' | 'member';
      joinedAt: number;
      addedBy: string;
      nodeInfo?: {
        peerId?: string;
        addresses: string[];
      };
    }>;
  }>>;
}

export interface PluginDocAPI {
  get: <T extends Record<string, unknown> = Record<string, unknown>>(collection: string, id: string) => Promise<T | null>;
  /** 声明集合同步策略：写入前必须调用，syncStrategy 必填；声明持久化且不可变更 */
  defineCollection: (collection: string, schema: PluginCollectionSchema) => Promise<PluginDeclaredCollectionSchema>;
  put: (collection: string, id: string, doc: Record<string, unknown>) => Promise<{ success: boolean }>;
  delete: (collection: string, id: string) => Promise<{ success: boolean }>;
  query: <T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    options?: PluginDocQueryOptions
  ) => Promise<{
    items: Array<{ id: string; data: T }>;
    nextCursor?: string;
  }>;
}

/**
 * 插件身份能力
 * 签名使用调用方插件域身份（域私钥永不离开内核），根身份不暴露；
 * 验签为纯函数，可用于校验其他成员在对应域内的签名
 */
export interface PluginIdentityAPI {
  sign: (payload: string) => Promise<DomainSignature>;
  verify: (payload: string, signature: string, publicKey: string) => Promise<{ valid: boolean }>;
}

/**
 * 集合同步策略声明（设计文档 V2 §4.3.4）
 * - `syncStrategy` 必填，类型层面强制显式选择：
 *   - `append-only`（默认推荐）：仅追加、不覆盖、不删除，自动配合链式存证
 *   - `lww`：最后写入获胜，仅适用于可容忍覆盖的普通状态数据
 * - `governance`：治理类数据（投票、成员、账目）标记，强制 append-only + 链式存证，插件无权降级
 * - `enableEvidence`：仅 lww 集合可选；append-only 集合强制开启
 * 声明持久化且不可变更，重复声明必须与首次一致。
 */
export interface PluginCollectionSchema {
  syncStrategy: 'append-only' | 'lww';
  governance?: boolean;
  enableEvidence?: boolean;
}

export interface PluginDeclaredCollectionSchema {
  collection: string;
  syncStrategy: 'append-only' | 'lww';
  governance: boolean;
  enableEvidence: boolean;
}

export interface PluginSDK {
  /** 当前插件的域身份：tab 模式下由 URL query `pluginDomain` 解析（对齐旧 tab 语义） */
  domain: string;
  evidence: PluginEvidenceAPI;
  p2p: PluginP2PAPI;
  runtime: PluginRuntimeAPI;
  docs: PluginDocAPI;
  identity: PluginIdentityAPI;
}

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
