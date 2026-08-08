/**
 * plugin/source 宿主 srcdoc 生成测试。
 * 重点回归：CSP 来源必须是插件源 origin（不含 id 路径段）——CSP 路径匹配规则下，
 * 不带尾斜杠的路径只精确匹配自身，会把 `/<id>/views/main.js` 误挡（真机踩过的坑）。
 */
import { describe, expect, it, vi } from 'vitest';

vi.mock('../../api', () => ({
  isTauri: () => true
}));

import { buildPluginHostSrcdoc, pluginSourceBaseUrl } from '../../plugin/source';

describe('buildPluginHostSrcdoc', () => {
  it('CSP 来源为插件源 origin，不带插件 id 路径段', () => {
    const srcdoc = buildPluginHostSrcdoc('spark-example');
    // jsdom UA 非 Windows：源形态为 plugin://localhost/<id>
    expect(pluginSourceBaseUrl('spark-example')).toBe('plugin://localhost/spark-example');
    // script-src/style-src/connect-src/img-src/font-src 均应为 origin 级
    expect(srcdoc).toContain("script-src plugin://localhost;");
    expect(srcdoc).toContain("style-src plugin://localhost 'unsafe-inline'");
    expect(srcdoc).not.toContain('script-src plugin://localhost/spark-example');
    expect(srcdoc).not.toContain('style-src plugin://localhost/spark-example');
    // bundle/css 引用仍带完整 id 路径
    expect(srcdoc).toContain('src="plugin://localhost/spark-example/views/main.js"');
    expect(srcdoc).toContain('href="plugin://localhost/spark-example/assets/main.css"');
  });

  it('repo 形态 id：origin 提取不受编码段影响', () => {
    const srcdoc = buildPluginHostSrcdoc('github.com/owner/repo');
    expect(srcdoc).toContain("script-src plugin://localhost;");
    expect(srcdoc).not.toContain('script-src plugin://localhost/github.com');
  });

  it('注入 mount 引导信息时 script-src 追加 unsafe-inline', () => {
    const srcdoc = buildPluginHostSrcdoc('spark-example', { viewId: 'post-card', viewType: 'message-card' } as never);
    expect(srcdoc).toContain("script-src plugin://localhost 'unsafe-inline'");
    expect(srcdoc).toContain('window.__sparkPluginView');
  });

  it('非法 id 直接拒绝生成', () => {
    expect(() => buildPluginHostSrcdoc('bad"id')).toThrow('Invalid plugin id');
    expect(() => buildPluginHostSrcdoc('../etc')).toThrow('Invalid plugin id');
  });
});
