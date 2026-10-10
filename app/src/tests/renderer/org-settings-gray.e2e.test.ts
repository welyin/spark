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
// SettingsPage 的重依赖打占位（本文件只验组织管理灰度接线，不验个人设置各模块）
vi.mock('../../components/mine/ProfileModule.vue', () => ({ default: { name: 'ProfileModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/MyCardModule.vue', () => ({ default: { name: 'MyCardModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/BackupModule.vue', () => ({ default: { name: 'BackupModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/DevicesModule.vue', () => ({ default: { name: 'DevicesModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/StorageModule.vue', () => ({ default: { name: 'StorageModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/PermissionModule.vue', () => ({ default: { name: 'PermissionModuleStub', template: '<div />' } }));
vi.mock('../../components/mine/SecurityModule.vue', () => ({ default: { name: 'SecurityModuleStub', template: '<div />' } }));
vi.mock('../../components/settings/SystemSettingsPanel.vue', () => ({
  default: { name: 'SystemSettingsPanelStub', template: '<div />' }
}));
vi.mock('../../components/UserAvatar.vue', () => ({ default: { name: 'UserAvatarStub', template: '<div />' } }));
vi.mock('../../components/OrgAvatar.vue', () => ({ default: { name: 'OrgAvatarStub', template: '<div />' } }));
vi.mock('../../components/MobileBackBar.vue', () => ({ default: { name: 'MobileBackBarStub', template: '<div />' } }));
// 移动导航栈转场容器：须渲染默认插槽，移动端分支才可见
vi.mock('../../components/MobilePageTransition.vue', () => ({
  default: { name: 'MobilePageTransitionStub', template: '<div><slot /></div>' }
}));

import ElementPlus from 'element-plus';
import SpaceSettingsDialog from '../../components/topnav/SpaceSettingsDialog.vue';
import SettingsPage from '../../pages/SettingsPage.vue';
import { BUILTIN_APPS, builtinImpl, builtinPluginFor, setBuiltinImpl } from '../../stores/builtin-apps';
import { switchToOrg, switchToPersonal } from '../../stores/current-space';
import { isMobileLayout } from '../../stores/ui-layout';
import { pushPage, resetStack } from '../../stores/mobile-nav';

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

describe('SettingsPage 组织管理灰度切换（A42 评审建议：mobile/desktop 两模板分支覆盖）', () => {
  function mountSettings(): HTMLElement {
    const host = document.createElement('div');
    document.body.appendChild(host);
    const app = createApp({ render: () => h(SettingsPage) });
    app.use(ElementPlus);
    app.mount(host);
    mountedApp = app;
    return host;
  }

  afterEach(() => {
    resetStack('settings');
    isMobileLayout.value = false;
  });

  it('desktop 分支：默认 legacy 旧面板；切插件版改挂 spark-org-admin，切回恢复', async () => {
    const host = mountSettings();
    await flush();
    // 组织空间默认选中「组织设置」（activeMenu='space'），desktop 分支直接渲染
    expect(host.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(host.querySelector('.stub-plugin-host')).toBeNull();
    setBuiltinImpl('org', 'plugin');
    await flush();
    const stub = host.querySelector('.stub-plugin-host');
    expect(stub?.getAttribute('data-plugin-id')).toBe('spark-org-admin');
    expect(stub?.getAttribute('data-view-id')).toBe('default');
    expect(host.querySelector('.stub-org-settings-panel')).toBeNull();
    setBuiltinImpl('org', 'legacy');
    await flush();
    expect(host.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(host.querySelector('.stub-plugin-host')).toBeNull();
  });

  it('desktop 分支：插件版加载失败「关闭」回退旧 UI 且持久化回 legacy（onOrgFallback）', async () => {
    const host = mountSettings();
    await flush();
    setBuiltinImpl('org', 'plugin');
    await flush();
    const stub = host.querySelector('.stub-plugin-host');
    expect(stub).not.toBeNull();
    (stub as any).__vueParentComponent.emit('close');
    await flush();
    expect(host.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(builtinImpl('org')).toBe('legacy');
    expect(localStorage.getItem('spark:builtin-impl:org')).toBe('legacy');
  });

  it('mobile 分支：导航栈进「组织设置」分组页后同一灰度接线生效', async () => {
    isMobileLayout.value = true;
    const host = mountSettings();
    await flush();
    // 栈1 菜单页不渲染设置面板；压入「组织设置」分组页（栈2）后出现
    pushPage('settings', 'section', { key: 'space' });
    await flush();
    expect(host.querySelector('.stub-org-settings-panel')).not.toBeNull();
    setBuiltinImpl('org', 'plugin');
    await flush();
    const stub = host.querySelector('.stub-plugin-host');
    expect(stub?.getAttribute('data-plugin-id')).toBe('spark-org-admin');
    expect(host.querySelector('.stub-org-settings-panel')).toBeNull();
    // mobile 分支同一回退：close → 写回 legacy
    (stub as any).__vueParentComponent.emit('close');
    await flush();
    expect(host.querySelector('.stub-org-settings-panel')).not.toBeNull();
    expect(builtinImpl('org')).toBe('legacy');
  });
});
