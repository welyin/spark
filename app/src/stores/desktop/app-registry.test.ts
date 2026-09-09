import { describe, expect, it, vi } from 'vitest';

vi.mock('../../mock/mode', () => ({ mockMode: () => true }));
vi.mock('../../mock/apps', () => ({
  listMockApps: () => [
    { id: 'personal-app', domain: 'plugin:personal-app', name: 'Personal', installed: true, supportedSpaces: ['personal'], views: ['main'] },
    { id: 'org-app', domain: 'plugin:org-app', name: 'Org', installed: true, supportedSpaces: ['org'], views: ['main'] },
    { id: 'not-installed', domain: 'plugin:not-installed', name: 'Available', installed: false, supportedSpaces: ['personal'], views: ['main'] },
    // manifest window 声明：窄高形（如 AI 聊天插件 480×680）
    { id: 'narrow-app', domain: 'plugin:narrow-app', name: 'Narrow', installed: true, supportedSpaces: ['personal'], views: ['main'], window: { defaultWidth: 480, defaultHeight: 680 } },
    // 非法声明（越界 / 缺维度语义由壳层归一化为未声明；此处验证桥后二次防卫回退）
    { id: 'bad-window-app', domain: 'plugin:bad-window-app', name: 'BadWindow', installed: true, supportedSpaces: ['personal'], views: ['main'], window: { defaultWidth: 100, defaultHeight: 680 } }
  ]
}));
vi.mock('../current-space', async () => {
  const { ref } = await import('vue');
  return { currentSpace: ref({ type: 'personal' }) };
});

import { appList, getApp } from './app-registry';

describe('desktop app registry', () => {
  it('includes installed mock apps without exposing other spaces or uninstalled apps', () => {
    // appList 按名称 zh 序排列（BadWindow < Narrow < Personal）
    expect(appList.value.map((app) => app.id)).toEqual(['bad-window-app', 'narrow-app', 'personal-app']);
    expect(appList.value.find((app) => app.id === 'personal-app')?.view).toBe('main');
  });

  it('窗口默认尺寸：未声明回退 880×620', () => {
    const app = getApp('personal-app', 'personal');
    expect(app?.defaultWidth).toBe(880);
    expect(app?.defaultHeight).toBe(620);
  });

  it('窗口默认尺寸：manifest window 声明覆盖默认（插件级，480×680）', () => {
    const app = getApp('narrow-app', 'personal');
    expect(app?.defaultWidth).toBe(480);
    expect(app?.defaultHeight).toBe(680);
  });

  it('窗口默认尺寸：非法声明（宽 100 越界）回退 880×620', () => {
    const app = getApp('bad-window-app', 'personal');
    expect(app?.defaultWidth).toBe(880);
    expect(app?.defaultHeight).toBe(620);
  });
});
