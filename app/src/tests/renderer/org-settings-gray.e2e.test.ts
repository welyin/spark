// 组织管理界面灰度切换 e2e（A42，与 app-builtin-gray.e2e.test.ts 同口径）：
// SpaceSettingsDialog（顶栏空间设置对话框，组织设置界面宿主之一）经
// BuiltinAppHost 在「旧内置 OrgSettingsPanel」与「默认内置插件版
// spark-org-admin」之间切换：
// - 默认（无持久化选择）渲染旧内置 UI；
// - 切到 plugin 后改挂 spark-org-admin 插件宿主，切回 legacy 恢复；
// - 插件版加载失败「关闭」（close 事件）回退旧 UI 且持久化回 legacy；
// - 注册表含 org 注册项（spark-org-admin/default 视图）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';

vi.mock('element-plus', async (importOriginal) => {
  const original = await importOriginal<typeof import('element-plus')>();
  return {
    ...original,
    ElMessage: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() })
  };
});
// 旧内置组织设置面板打占位（模板文本区分 legacy / 插件宿主）
vi.mock('../../components/org/OrgSettingsPanel.vue', () => ({
  default: { name: 'OrgSettingsPanelStub', template: '<div class="stub-org-settings-panel" />' }
}));
// 插件宿主打占位：记录 props 并暴露 close 触发
vi.mock('../../components/plugin/PluginIframeHost.vue', () => ({
  default: {
    name: 'PluginIframeHostStub',
    props: ['pluginId', 'viewId', 'space'],
    emits: ['close', 'manifest'],
    template: '<div class="stub-plugin-host" :data-plugin-id="pluginId" :data-view-id="viewId" />'
  }
}));

import ElementPlus from 'element-plus';
import SpaceSettingsDialog from '../../components/topnav/SpaceSettingsDialog.vue';
import { BUILTIN_APPS, builtinImpl, builtinPluginFor, setBuiltinImpl } from '../../stores/builtin-apps';
import { switchToOrg, switchToPersonal } from '../../stores/current-space';

beforeEach(() => {
  localStorage.clear();
  setBuiltinImpl('org', 'legacy');
  switchToOrg('org_gray');
  vi.clearAllMocks();
});

let mountedApp: ReturnType<typeof createApp> | null = null;

afterEach(() => {
  mountedApp?.unmount();
  mountedApp = null;
  switchToPersonal();
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

function mountDialog(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(SpaceSettingsDialog, { modelValue: true }) });
  app.use(ElementPlus);
  app.mount(host);
  mountedApp = app;
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('组织管理灰度注册表（A42）', () => {
  it('org 注册项指向 spark-org-admin 默认视图；未登记 tab 恒 legacy', () => {
    const def = builtinPluginFor('org');
    expect(def?.pluginId).toBe('spark-org-admin');
    expect(def?.viewId).toBe('default');
    expect(BUILTIN_APPS.map((item) => item.tabId)).toContain('org');
    // 默认 legacy；切 plugin 生效并持久化
    expect(builtinImpl('org')).toBe('legacy');
    setBuiltinImpl('org', 'plugin');
    expect(builtinImpl('org')).toBe('plugin');
    expect(localStorage.getItem('spark:builtin-impl:org')).toBe('plugin');
  });
});

describe('SpaceSettingsDialog 组织管理灰度切换（A42）', () => {
  it('默认渲染旧内置组织设置面板（无插件宿主）', async () => {
    const host = mountDialog();
    await flush();
    expect(document.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(document.querySelector('.stub-plugin-host')).toBeNull();
  });

  it('切到插件版：改挂 spark-org-admin 插件宿主，切回 legacy 恢复旧 UI', async () => {
    mountDialog();
    await flush();
    setBuiltinImpl('org', 'plugin');
    await flush();
    const pluginHost = document.querySelector('.stub-plugin-host');
    expect(pluginHost?.getAttribute('data-plugin-id')).toBe('spark-org-admin');
    expect(pluginHost?.getAttribute('data-view-id')).toBe('default');
    expect(document.querySelector('.stub-org-settings-panel')).toBeNull();
    setBuiltinImpl('org', 'legacy');
    await flush();
    expect(document.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(document.querySelector('.stub-plugin-host')).toBeNull();
  });

  it('插件版加载失败「关闭」：回退旧 UI 且持久化回 legacy', async () => {
    mountDialog();
    await flush();
    setBuiltinImpl('org', 'plugin');
    await flush();
    const stub = document.querySelector('.stub-plugin-host');
    expect(stub).not.toBeNull();
    (stub as any).__vueParentComponent.emit('close');
    await flush();
    expect(document.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(builtinImpl('org')).toBe('legacy');
    expect(localStorage.getItem('spark:builtin-impl:org')).toBe('legacy');
  });

  it('切回个人空间：对话框自动关闭（不渲染任何界面实现）', async () => {
    mountDialog();
    await flush();
    expect(document.querySelector('.stub-org-settings-panel')).not.toBeNull();
    switchToPersonal();
    await flush();
    expect(document.querySelector('.stub-org-settings-panel')).toBeNull();
    expect(document.querySelector('.stub-plugin-host')).toBeNull();
  });
});
