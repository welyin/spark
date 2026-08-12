/**
 * plugin/source 宿主 srcdoc 生成与平台分支测试。
 * 重点回归：
 * - CSP 来源必须是插件源 origin（不含 id 路径段）——CSP 路径匹配规则下，
 *   不带尾斜杠的路径只精确匹配自身，会把 `/<id>/views/main.js` 误挡（真机踩过的坑）；
 * - 平台分支（plugin_decoupling.md §5.4/5.5）：Windows / Android 走
 *   `http://plugin.localhost`，macOS / iOS 走 `plugin://localhost`，dev 走 vite 中间件。
 */
import { describe, expect, it, vi } from 'vitest';

vi.mock('../../api', () => ({
  isTauri: () => true
}));

import {
  buildPluginHostSrcdoc,
  pluginSourceBaseUrl,
  resolvePluginBaseUrl
} from '../../plugin/source';

describe('resolvePluginBaseUrl 平台分支', () => {
  const encoded = 'spark-example';
  const origin = 'http://192.168.1.50:1420';

  it('Windows + 生产：http://plugin.localhost', () => {
    expect(
      resolvePluginBaseUrl('spark-example', {
        encoded,
        tauri: true,
        dev: false,
        origin,
        userAgent: 'Mozilla/5.0 (Windows NT 10.0)'
      })
    ).toBe('http://plugin.localhost/spark-example');
  });

  it('Android + 生产：http://plugin.localhost（真机加载失败的既有 bug 修复）', () => {
    expect(
      resolvePluginBaseUrl('spark-example', {
        encoded,
        tauri: true,
        dev: false,
        origin,
        userAgent: 'Mozilla/5.0 (Linux; Android 13; Pixel 7)'
      })
    ).toBe('http://plugin.localhost/spark-example');
  });

  it('macOS / iOS + 生产：plugin://localhost', () => {
    expect(
      resolvePluginBaseUrl('spark-example', {
        encoded,
        tauri: true,
        dev: false,
        origin,
        userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)'
      })
    ).toBe('plugin://localhost/spark-example');
  });

  it('dev（Tauri 真机/桌面）：优先 vite 中间件（局域网 IP），而非 http://plugin.localhost', () => {
    expect(
      resolvePluginBaseUrl('spark-example', {
        encoded,
        tauri: true,
        dev: true,
        origin,
        userAgent: 'Mozilla/5.0 (Linux; Android 13)'
      })
    ).toBe('http://192.168.1.50:1420/plugin/spark-example');
  });

  it('repo 形态 id：编码段收成单段，dev 中间件透传', () => {
    const repo = encodeURIComponent('github.com/welyin/spark/plugins/spark-moments');
    expect(
      resolvePluginBaseUrl('github.com/welyin/spark/plugins/spark-moments', {
        encoded: repo,
        tauri: false,
        dev: true,
        origin,
        userAgent: ''
      })
    ).toBe(`${origin}/plugin/${repo}`);
  });
});

describe('buildPluginHostSrcdoc（生产 wire 形态）', () => {
  // srcdoc 的 CSP/源形态只对生产生效；vitest 默认 dev 模式会把 pluginSourceBaseUrl
  // 打进 vite 中间件分支，故显式传 dev=false 强制走生产 wire 线形（§5.5 不变项）
  const dev = false;

  it('CSP 来源为插件源 origin，不带插件 id 路径段', () => {
    const srcdoc = buildPluginHostSrcdoc('spark-example', undefined, dev);
    // jsdom UA 非 Windows/Android：生产源形态为 plugin://localhost/<id>
    expect(pluginSourceBaseUrl('spark-example', dev)).toBe('plugin://localhost/spark-example');
    // script-src/style-src/connect-src/img-src/font-src 均应为 origin 级
    expect(srcdoc).toContain('script-src plugin://localhost;');
    expect(srcdoc).toContain("style-src plugin://localhost 'unsafe-inline'");
    expect(srcdoc).not.toContain('script-src plugin://localhost/spark-example');
    expect(srcdoc).not.toContain('style-src plugin://localhost/spark-example');
    // bundle/css 引用仍带完整 id 路径
    expect(srcdoc).toContain('src="plugin://localhost/spark-example/views/main.js"');
    expect(srcdoc).toContain('href="plugin://localhost/spark-example/assets/main.css"');
  });

  it('repo 形态 id：origin 提取不受编码段影响', () => {
    const srcdoc = buildPluginHostSrcdoc('github.com/owner/repo', undefined, dev);
    expect(srcdoc).toContain('script-src plugin://localhost;');
    expect(srcdoc).not.toContain('script-src plugin://localhost/github.com');
  });

  it('注入 mount 引导信息时 script-src 追加 unsafe-inline', () => {
    const srcdoc = buildPluginHostSrcdoc(
      'spark-example',
      {
        viewId: 'post-card',
        viewType: 'message-card'
      } as never,
      dev
    );
    expect(srcdoc).toContain("script-src plugin://localhost 'unsafe-inline'");
    expect(srcdoc).toContain('window.__sparkPluginView');
  });

  it('非法 id 直接拒绝生成', () => {
    expect(() => buildPluginHostSrcdoc('bad"id')).toThrow('Invalid plugin id');
    expect(() => buildPluginHostSrcdoc('../etc')).toThrow('Invalid plugin id');
  });
});
