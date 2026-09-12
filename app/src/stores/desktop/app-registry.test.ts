import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../../mock/mode', () => ({ mockMode: () => true }));
vi.mock('../../mock/apps', () => ({
  // enabled=true：个人空间 per-space 记录缺席时的过渡种子（内核全局开关口径，
  // 见 app-enablement defaultEnabledInSpace）；未启用项由 enabled:false / 记录覆盖
  listMockApps: () => [
    { id: 'personal-app', domain: 'plugin:personal-app', name: 'Personal', installed: true, enabled: true, supportedSpaces: ['personal'], views: ['main'] },
    { id: 'org-app', domain: 'plugin:org-app', name: 'Org', installed: true, enabled: true, supportedSpaces: ['org'], views: ['main'] },
    { id: 'not-installed', domain: 'plugin:not-installed', name: 'Available', installed: false, enabled: true, supportedSpaces: ['personal'], views: ['main'] },
    // 已装但本空间未启用（enabled:false 且无 per-space 记录）：不进注册表
    { id: 'disabled-app', domain: 'plugin:disabled-app', name: 'Disabled', installed: true, enabled: false, supportedSpaces: ['personal'], views: ['main'] },
    // manifest window 声明：窄高形（如 AI 聊天插件 480×680）
    { id: 'narrow-app', domain: 'plugin:narrow-app', name: 'Narrow', installed: true, enabled: true, supportedSpaces: ['personal'], views: ['main'], window: { defaultWidth: 480, defaultHeight: 680 } },
    // 非法声明（越界 / 缺维度语义由壳层归一化为未声明；此处验证桥后二次防卫回退）
    { id: 'bad-window-app', domain: 'plugin:bad-window-app', name: 'BadWindow', installed: true, enabled: true, supportedSpaces: ['personal'], views: ['main'], window: { defaultWidth: 100, defaultHeight: 680 } }
  ]
}));
vi.mock('../current-space', async () => {
  const { ref } = await import('vue');
  return { currentSpace: ref({ type: 'personal' }) };
});

import { appList, getApp } from './app-registry';
import { setAppEnabledInSpace } from '../app-enablement';

beforeEach(() => {
  localStorage.clear();
});

describe('desktop app registry', () => {
  it('注册表口径＝本空间可见且已启用（启用＝逻辑状态，不要求已安装，2026-09-10 形式化定义）', () => {
    // appList 按名称 zh 序排列（Available < BadWindow < Narrow < Personal）；
    // not-installed（已启用未安装）进注册表（打开时由插件宿主提示就地安装）；
    // org-app（本空间不可见）/ disabled-app（本空间未启用）不进注册表
    expect(appList.value.map((app) => app.id)).toEqual(['not-installed', 'bad-window-app', 'narrow-app', 'personal-app']);
    expect(appList.value.find((app) => app.id === 'personal-app')?.view).toBe('main');
  });

  it('per-space 启用记录生效：显式启用后进入注册表，停用后移出', () => {
    setAppEnabledInSpace({ type: 'personal' }, 'disabled-app', true);
    expect(appList.value.map((app) => app.id)).toContain('disabled-app');
    setAppEnabledInSpace({ type: 'personal' }, 'disabled-app', false);
    expect(appList.value.map((app) => app.id)).not.toContain('disabled-app');
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

  it('getApp 有意不查启用态（运行中窗口/Dock 标签在停用后仍需解析标题）', () => {
    expect(getApp('disabled-app', 'personal')?.name).toBe('Disabled');
  });
});
