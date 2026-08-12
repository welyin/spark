/**
 * 渲染端插件 SDK 后端（Tauri 版，移植自旧工程 desktop/src/renderer/plugin-sdk-browser.ts，
 * 现位于 src/plugin/sdk-browser.ts）。
 * 注：旧工程文件名含 plugin- 前缀，本目录已去除该前缀。
 *
 * 与旧版的差异仅在宿主来源：旧版经 Electron preload 暴露 window.electronAPI，
 * 本版由适配层（src/api/index.ts，installHostApi）在 Tauri 环境下安装同形实现。
 * 插件业务代码（service/view）零改动。
 *
 * 插件 iframe 沙箱化（阶段 A 第三波）后，旧 tab 同进程初始化（initializePluginSDK，
 * 按窗口/URL query 解析域并写全局注入点）已随壳层旧注册路径一并退役；本模块只保留
 * createPluginBackend——桥 dispatcher（./bridge-dispatcher.ts）以桥绑定的身份域
 * 构造后端，域一律显式下传给 plugin.* 命令。
 *
 * 类型（PluginSDK 等）的唯一来源是 @spark/plugin-sdk（code/packages/plugin-sdk，
 * 经相对路径引用），本模块 re-export 以保持既有 import 路径兼容。
 */

import type { ElectronAPI } from '../api';
import type { FetchStreamHandle, PluginSDK, SysFetchChunk } from '../../../packages/plugin-sdk/src';

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
} from '../../../packages/plugin-sdk/src';

// ------------------------------------------------------------------
// 后端构造（桥 dispatcher 使用）
// ------------------------------------------------------------------

declare global {
  interface Window {
    electronAPI: ElectronAPI;
  }
}

/**
 * 按域构造插件 SDK 后端。
 *
 * 桥模式：dispatcher 以桥绑定的身份域调用，主窗口无 tab URL 可回退，
 * 域一律显式传给 plugin.* 命令。
 *
 * @throws 如果宿主 API 不可用
 */
export function createPluginBackend(domain: string, orgId?: string): PluginSDK {
  // 沙箱化后后端只在壳层主窗口构造，无跨 frame 回退的合法场景
  const electronAPI: ElectronAPI | undefined = window.electronAPI;
  if (!electronAPI) {
    throw new Error('electronAPI is not available in the renderer context');
  }
  const pluginDomain = domain;
  // 插件 id（剥离域前缀 `plugin:`）：社交投递 feed 的 topic 前缀校验/归属用
  const boundPluginId = domain.startsWith('plugin:')
    ? domain.slice('plugin:'.length)
    : domain;
  // O3 org 上下文：插件运行在 org space 时注入 orgId（personal space 为
  // undefined），data.* 读写命令据此路由到 org 集合；内核按实例空间解析。
  const boundOrgId = orgId ?? undefined;

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
    // P6 声明式数据 API（写库即同步；插件侧零同步参数）
    data: {
      declareCollection: (declaration) => {
        // F8：桥 declareCollection orgId 绑定——插件实例绑定的 orgId（boundOrgId）
        // 是权威来源；自报 orgId 须与绑定一致（不一致拒绝，防插件向用户所属
        // 任意组织声明）。org space（boundOrgId 有值）注入绑定值供内核校验。
        const selfOrgId = (declaration as { orgId?: string } | undefined)?.orgId;
        if (selfOrgId !== undefined && selfOrgId !== boundOrgId) {
          throw new Error(
            `declareCollection orgId mismatch: plugin self-declared ${selfOrgId}, bound ${boundOrgId}`
          );
        }
        // 命令侧声明类型含可选 orgId；org space 注入绑定值（personal 不注入）。
        return electronAPI.plugin.dataDeclareCollection(
          boundOrgId !== undefined ? { ...declaration, orgId: boundOrgId } : declaration,
          pluginDomain
        );
      },
      save: (name, key, value, version) =>
        electronAPI.plugin.dataSave(name, key, value, version, boundOrgId, pluginDomain),
      delete: (name, key, version) =>
        electronAPI.plugin.dataDelete(name, key, version, boundOrgId, pluginDomain),
      get: (name, key, version) =>
        electronAPI.plugin.dataGet(name, key, version, boundOrgId, pluginDomain),
      query: (name, options = {}, version) =>
        electronAPI.plugin.dataQuery(name, options, version, boundOrgId, pluginDomain),
      dropVersion: (name, version) =>
        electronAPI.plugin.dataDropVersion(name, version, pluginDomain),
      saveBlob: (dataBase64) =>
        electronAPI.plugin.dataSaveBlob(dataBase64),
      readBlob: (hash) =>
        electronAPI.plugin.dataReadBlob(hash),
      // O4 encrypted 授权名单（owner 侧）：orgId 由桥绑定注入（boundOrgId）
      grantAccess: (name, members, version) =>
        electronAPI.plugin.dataGrantAccess(boundOrgId ?? '', name, version ?? '1', members),
      revokeAccess: (name, members, version) =>
        electronAPI.plugin.dataRevokeAccess(boundOrgId ?? '', name, version ?? '1', members),
      listAccess: (name, version) =>
        electronAPI.plugin.dataListAccess(boundOrgId ?? '', name, version ?? '1'),
      // iframe 侧远端合入通知由 PluginIframeHost 经桥事件通道实现；
      // 本后端（宿主内嵌 QuickJS 等直连接口）无该通路，以 no-op 满足契约
      onChange: async () => {}
    },
    identity: {
      sign: (payload: string) =>
        electronAPI.plugin.identitySign(payload, pluginDomain),
      verify: (payload: string, signature: string, publicKey: string) =>
        electronAPI.plugin.identityVerify(payload, signature, publicKey)
    },
    // 通讯录只读门面（social-feed §9.4 contact:read；权限由桥 dispatcher 强制）
    contacts: {
      listFriends: () => electronAPI.contacts.listFriends(),
      listGroups: () => electronAPI.contacts.listGroups(),
      listTags: () => electronAPI.contacts.listTags()
    },
    // 社交投递（social-feed §9.1 sdk.feed；deliver 权限由桥 dispatcher 强制，
    // onReceive/pull 接收侧免权限）。pluginId 由本后端按域注入（不信插件自报）。
    feed: {
      deliver: (input) =>
        electronAPI.feed.deliver(
          boundPluginId,
          input.topic,
          input.payload,
          input.recipients,
          input.replyTo,
          input.feedId
        ),
      pull: (input) =>
        electronAPI.feed.pull(boundPluginId, input.topic, input.cursor, input.limit),
      // 在线推送 onReceive 经桥事件通道（PluginIframeHost → bridge event）实现；
      // 本后端（宿主内嵌 QuickJS 等直连接口）无该通路，以 no-op 满足契约
      // （同 data.onChange 口径）
      onReceive: async () => {}
    },
    sys: electronAPI.sys
      ? {
          exec: (program: string, args: string[], workdir?: string) =>
            electronAPI.sys.exec(program, args, workdir),
          fetch: (url: string, options?: Record<string, unknown>) =>
            electronAPI.sys.fetch(url, options as { method?: string; headers?: Record<string, string>; body?: string } | undefined),
          // 目录选择对话框（纯前端 tauri-plugin-dialog，宿主 api.sys.pickFolder）
          pickFolder: (title?: string) => electronAPI.sys.pickFolder(title),
          // 内嵌后端（非 iframe 桥，无桥 events 通道）的 fetchStream：直接
          // 用 Tauri listen 订阅 `sys-stream:{streamId}` 事件实现完整 handle
          // （done/onChunk/cancel），不再返回只含 streamId 的残次对象。
          // dispatcher 场景另自 listen 转发为桥 event，此处 handle 供非桥
          // 调用方直接消费，两者各自订阅互不干扰。
          fetchStream: async (url, options): Promise<FetchStreamHandle> => {
            const { streamId } = await electronAPI.sys.fetchStream(
              url,
              options as { method?: string; headers?: Record<string, string>; body?: string } | undefined
            );
            const event = `sys-stream:${streamId}`;
            const { listen } = await import('@tauri-apps/api/event');
            const handlers = new Set<(chunk: SysFetchChunk) => void>();
            let unlisten: (() => void) | null = null;
            const done = new Promise<SysFetchChunk>((resolve, reject) => {
              void listen<SysFetchChunk>(event, (e) => {
                const c = e.payload;
                if (c.done) {
                  unlisten?.();
                  if (c.status === 0) {
                    // status:0 哨兵 = 内核侧请求失败（错误文案在 text 中）
                    reject(new Error(c.text));
                  } else {
                    resolve(c);
                  }
                } else {
                  for (const handler of [...handlers]) {
                    handler(c);
                  }
                }
              })
                .then((u) => {
                  unlisten = u;
                })
                .catch(reject);
            });
            return {
              streamId,
              done,
              onChunk(handler) {
                handlers.add(handler);
              },
              cancel() {
                unlisten?.();
              }
            };
          }
        }
      : undefined
  };
}

