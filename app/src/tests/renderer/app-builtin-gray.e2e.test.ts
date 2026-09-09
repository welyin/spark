// App.vue 默认内置应用灰度切换 e2e（communication §五.3，A19 验收：灰度切换 e2e）。
//
// 覆盖：
// - 默认（无持久化选择）渲染旧内置 UI（MessagesPage/ContactsPage）；
// - 切换为 plugin 后主 tab 改挂默认内置插件（PluginIframeHost，spark-chat /
//   spark-contacts），切回 legacy 恢复旧 UI——同一消息数据（sled）零迁移，
//   本测试验证的是壳层挂载面切换无感可用；
// - 选择持久化 localStorage（重进仍插件版）；
// - 插件版加载失败「关闭」（close 事件）回退旧 UI 且持久化回 legacy。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';

// --- 必须先于 App.vue 的 import 提升 mock 生效 ---
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {}))
}));
vi.mock('element-plus', async (importOriginal) => {
  const original = await importOriginal<typeof import('element-plus')>();
  return {
    ...original,
    ElMessage: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() })
  };
});
vi.mock('@tauri-apps/api/app', () => ({
  onBackButtonPress: vi.fn(() => Promise.resolve(() => {}))
}));
vi.mock('@element-plus/icons-vue', async () => {
  const actual = await vi.importActual<typeof import('@element-plus/icons-vue')>('@element-plus/icons-vue');
  return { ...actual };
});
// 页面/导航重依赖打成可识别占位（模板文本区分 legacy / 插件宿主）
vi.mock('../../pages/MessagesPage.vue', () => ({
  default: { name: 'MessagesPageStub', template: '<div class="stub-messages-page" />' }
}));
vi.mock('../../pages/ContactsPage.vue', () => ({
  default: { name: 'ContactsPageStub', template: '<div class="stub-contacts-page" />' }
}));
vi.mock('../../pages/AppsPage.vue', () => ({ default: { name: 'AppsPageStub', template: '<div />' } }));
vi.mock('../../pages/TestPage.vue', () => ({ default: { name: 'TestPageStub', template: '<div />' } }));
vi.mock('../../pages/SettingsPage.vue', () => ({ default: { name: 'SettingsPageStub', template: '<div />' } }));
vi.mock('../../pages/MinePage.vue', () => ({ default: { name: 'MinePageStub', template: '<div />' } }));
vi.mock('../../components/TopNavbar.vue', () => ({ default: { name: 'TopNavbarStub', template: '<div />' } }));
vi.mock('../../components/UserAvatarMenu.vue', () => ({ default: { name: 'UserAvatarMenuStub', template: '<div />' } }));
vi.mock('../../components/MobileTabBar.vue', () => ({ default: { name: 'MobileTabBarStub', template: '<div />' } }));
vi.mock('../../components/MobileTopBar.vue', () => ({ default: { name: 'MobileTopBarStub', template: '<div />' } }));
vi.mock('../../components/MobileSpaceDrawer.vue', () => ({ default: { name: 'MobileSpaceDrawerStub', template: '<div />' } }));
// 插件宿主打占位：记录 props 与暴露 close 触发（模板标记 pluginId 便于断言）
vi.mock('../../components/plugin/PluginIframeHost.vue', () => ({
  default: {
    name: 'PluginIframeHostStub',
    props: ['pluginId', 'viewId', 'space'],
    emits: ['close', 'manifest'],
    template: '<div class="stub-plugin-host" :data-plugin-id="pluginId" />'
  }
}));
vi.mock('../../plugin/source', () => ({ fetchPluginManifest: vi.fn(async () => null) }));
vi.mock('../../stores/current-user', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../stores/current-user')>();
  return { ...actual, refreshCurrentUser: vi.fn(async () => {}) };
});

import ElementPlus from 'element-plus';
import App from '../../App.vue';
import { currentUser } from '../../stores/current-user';
import { builtinImpl, setBuiltinImpl } from '../../stores/builtin-apps';

beforeEach(() => {
  localStorage.clear();
  setBuiltinImpl('messages', 'legacy');
  setBuiltinImpl('contacts', 'legacy');
  currentUser.rootId = 'root-gray';
  vi.clearAllMocks();
  (window as any).electronAPI = {
    system: { exitApp: vi.fn().mockResolvedValue(undefined) },
    rootIdentity: { status: vi.fn().mockResolvedValue({ initialized: true, unlocked: true, rootId: 'root-gray', nickname: 'u', avatar: null }) },
    organization: { listMine: vi.fn().mockResolvedValue([]) },
    pluginMarket: { list: vi.fn().mockResolvedValue([]) },
    p2p: {
      info: vi.fn().mockResolvedValue({
        initialized: true, started: true, peerId: 'peer-test', addresses: [], connectedPeers: [],
        sparkSyncSubscribers: [], error: null
      })
    },
    devices: { list: vi.fn().mockResolvedValue([]) },
    updater: { status: vi.fn().mockResolvedValue(null) }
  };
});

let mountedApp: ReturnType<typeof createApp> | null = null;

afterEach(() => {
  mountedApp?.unmount();
  mountedApp = null;
  currentUser.rootId = null;
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

function mountApp(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(App) });
  app.use(ElementPlus);
  app.mount(host);
  mountedApp = app;
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** PC 壳层页面已窗口化：点 rail「全部消息」在当前桌面开消息窗口 */
async function openMessagesWindow(host: HTMLElement): Promise<void> {
  const btn = host.querySelector<HTMLElement>('.rail-main .rail-item[title="全部消息"]');
  expect(btn).not.toBeNull();
  btn!.click();
  await flush();
}

describe('App.vue 默认内置应用灰度切换（A19）', () => {
  it('默认渲染旧内置 UI（消息窗口 = MessagesPage）', async () => {
    const host = mountApp();
    await flush();
    // PC 默认落桌面（无消息页），点「全部消息」后窗口内为 legacy 消息页
    expect(host.querySelector('.stub-messages-page')).toBeNull();
    await openMessagesWindow(host);
    expect(host.querySelector('.window-frame .stub-messages-page')).not.toBeNull();
    expect(host.querySelector('.stub-plugin-host')).toBeNull();
  });

  it('切到插件版：消息窗口改挂 spark-chat 插件宿主，切回 legacy 恢复旧 UI', async () => {
    const host = mountApp();
    await flush();
    await openMessagesWindow(host);
    setBuiltinImpl('messages', 'plugin');
    await flush();
    const pluginHost = host.querySelector('.stub-plugin-host');
    expect(pluginHost?.getAttribute('data-plugin-id')).toBe('spark-chat');
    expect(host.querySelector('.stub-messages-page')).toBeNull();
    // 切回旧 UI
    setBuiltinImpl('messages', 'legacy');
    await flush();
    expect(host.querySelector('.stub-messages-page')).not.toBeNull();
    expect(host.querySelector('.stub-plugin-host')).toBeNull();
  });

  it('通讯录已转为空间桌面插件窗口：无壳层主 tab 灰度面（docs/ui 阶段 3）', async () => {
    const host = mountApp();
    await flush();
    // rail 一级入口为 我的/消息/空间/事务——通讯录不在其中（经空间桌面
    // openPluginTab 打开 spark-contacts 插件窗口，无 legacy 壳层面）
    const railLabels = Array.from(host.querySelectorAll('.rail-main .rail-item .rail-label')).map(
      (el) => el.textContent
    );
    expect(railLabels).not.toContain('通讯录');
    // 灰度注册表只剩 messages——contacts 的开关写入被拒绝（未登记 tab 恒 legacy）
    setBuiltinImpl('contacts', 'plugin');
    expect(builtinImpl('contacts')).toBe('legacy');
    expect(builtinImpl('messages')).toBe('legacy');
  });

  it('选择持久化：localStorage 记录后重进仍为插件版', async () => {
    mountApp();
    await flush();
    setBuiltinImpl('messages', 'plugin');
    expect(localStorage.getItem('spark:builtin-impl:messages')).toBe('plugin');
    expect(builtinImpl('messages')).toBe('plugin');
  });

  it('插件版加载失败「关闭」：回退旧 UI 且持久化回 legacy', async () => {
    const host = mountApp();
    await flush();
    await openMessagesWindow(host);
    setBuiltinImpl('messages', 'plugin');
    await flush();
    const stub = host.querySelector('.stub-plugin-host');
    expect(stub).not.toBeNull();
    // 模拟 PluginIframeHost 发 close（加载失败「关闭」按钮）
    const vueInstance = (stub as any).__vueParentComponent;
    vueInstance.emit('close');
    await flush();
    expect(host.querySelector('.stub-messages-page')).not.toBeNull();
    expect(builtinImpl('messages')).toBe('legacy');
    expect(localStorage.getItem('spark:builtin-impl:messages')).toBe('legacy');
  });
});
