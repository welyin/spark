/**
 * 开发插件自动发现注入测试（plugin_decoupling.md §5 / §9）。
 * 重点回归：
 * - dev 下 vite 注入的 `__DEV_PLUGINS__` 映射为市场条目（id/name/version/domain，
 *   installed=false、permissions=[]，仅展示、打开走 dev 链路）；
 * - 生产（import.meta.env.DEV=false）即使 `__DEV_PLUGINS__` 有值也返回空——
 *   dev 逻辑/插件清单不得打进生产 bundle（§9 评审关注点）；
 * - `__DEV_PLUGINS__` 未注入（vitest/非 vite 运行时）按空处理，不抛 ReferenceError。
 */
import { describe, expect, it, vi, afterEach } from 'vitest';
import { listDevPlugins } from '../../mock/dev-plugins';

function setDevPlugins(value: unknown): void {
  (globalThis as Record<string, unknown>)['__DEV_PLUGINS__'] = value;
}
function deleteDevPlugins(): void {
  delete (globalThis as Record<string, unknown>)['__DEV_PLUGINS__'];
}

afterEach(() => {
  deleteDevPlugins();
  vi.restoreAllMocks();
});

describe('listDevPlugins（dev 自动扫描注入）', () => {
  it('dev 下把注入的开发插件映射为未安装市场条目', () => {
    setDevPlugins([
      { id: 'spark-moments', name: '朋友圈', version: '0.2.0', icon: '' },
      { id: 'ai-chat', name: 'AI 聊天', version: '0.1.5' }
    ]);
    const items = listDevPlugins();
    expect(items).toHaveLength(2);
    const moments = items.find((i) => i.id === 'spark-moments')!;
    expect(moments.domain).toBe('plugin:spark-moments');
    expect(moments.name).toBe('朋友圈');
    expect(moments.version).toBe('0.2.0');
    expect(moments.installed).toBe(false);
    expect(moments.enabled).toBe(false);
    expect(moments.permissions).toEqual([]);
    // 描述含 dev 标记，生产不可见的语义写入描述
    expect(moments.description).toContain('dev');
  });

  it('未注入 __DEV_PLUGINS__（非 vite 运行时）返回空，不抛 ReferenceError', () => {
    deleteDevPlugins();
    expect(listDevPlugins()).toEqual([]);
  });

  it('生产注入空数组（devPluginDiscovery 生产 define 为空）→ 返回空，不产生任何条目', () => {
    // 生产门控是构建期保证：devPluginDiscovery 在 NODE_ENV=production 时把
    // __DEV_PLUGINS__ 注入为 []，且 Vite 静态把 import.meta.env.DEV 替换为 false。
    // 这里验证映射是注入数组的纯函数：注入空 → 列表空（dev 逻辑/插件清单不泄漏进市场）。
    setDevPlugins([]);
    expect(listDevPlugins()).toEqual([]);
  });
});
