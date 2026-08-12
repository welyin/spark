/**
 * 开发插件自动发现（plugin_decoupling.md §5）：把 vite 侧扫描到的
 * code/plugins 下各子目录的 manifest.json 开发插件（经 `__DEV_PLUGINS__`
 * 注入）映射为市场条目，在 dev 链路下与真实市场结果合并展示。
 *
 * 门控：仅 `import.meta.env.DEV`（dev / tauri:mock）生效；生产构建下 Vite 把
 * `import.meta.env.DEV` 替换为 `false`，此模块整体被 tree-shake 出生产 bundle，
 * 且 `__DEV_PLUGINS__` 在生产构建注入空数组（双重保证 dev 逻辑不进生产，§9）。
 *
 * 资源基地址：dev 下插件 iframe 经 vite 中间件 `/plugin/<id>/` 加载（与壳层
 * 同 origin），此处只需提供市场条目元数据，不硬编码 URL（见 plugin/source.ts）。
 */
import type { PluginMarketItemDto } from '../api/types';

/** 由 vite 插件 devPluginDiscovery 注入的开发插件元数据（生产为空数组）。 */
declare const __DEV_PLUGINS__: ReadonlyArray<{ id: string; name: string; version: string; icon?: string }>;

/** 开发插件市场条目：未安装、未启用，仅供市场/列表展示，打开走 dev 链路。 */
function toDevMarketItem(plugin: { id: string; name: string; version: string }): PluginMarketItemDto {
  return {
    id: plugin.id,
    domain: `plugin:${plugin.id}`,
    name: plugin.name,
    description: `本地开发插件「${plugin.name}」（dev 自动扫描，生产不可见）`,
    category: 'tool',
    version: plugin.version,
    views: ['default'],
    permissions: [],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: '', installCommand: '' },
    installed: false,
    enabled: false,
    installedVersion: null,
    latestVersion: plugin.version,
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: 'not-checked',
    grantedPermissions: []
  };
}

/** 开发插件市场条目列表（仅 dev；生产返回空）。 */
export function listDevPlugins(): PluginMarketItemDto[] {
  if (!import.meta.env.DEV) {
    return [];
  }
  // vite 生产/开发已注入 `__DEV_PLUGINS__`；vitest/非 vite 运行时未注入（undefined）
  // 时按空处理，避免 ReferenceError
  if (typeof __DEV_PLUGINS__ === 'undefined') {
    return [];
  }
  return __DEV_PLUGINS__.map(toDevMarketItem);
}
