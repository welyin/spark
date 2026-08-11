// App.vue 主界面 M1 新设备通知端到端（DeviceNoticeReceived → ElMessage 警示）。
//
// 覆盖方案文档 §6.16（App.vue 分支）：
// - 收到 { kind:'DeviceNoticeReceived', data:{ kind:'device_joined', deviceId,
//   deviceName, ts } } → 走 handleDeviceNotice 判定为新设备 → ElMessage warning，
//   文案含设备名与「如非本人操作请立即在设备管理中撤销」。
//
// 手段：mock @tauri-apps/api/event 的 listen 捕获 App.vue 注册的 handler，
// 手动派发 DeviceNoticeReceived；App.vue 重依赖（页面/顶栏/插件宿主）打成占位。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';

// --- 必须先于 App.vue 的 import 提升 mock 生效 ---
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((channel: string, handler: (event: { payload: unknown }) => void) => {
    captured[channel] = handler;
    return Promise.resolve(() => {});
  })
}));
// ElMessage 对象式调用（ElMessage({type,message})）→ 打成可调用 fn 便于断言；
// 其余 element-plus 保留真实实现（app.use(ElementPlus) 挂载需要）。
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
vi.mock('@element-plus/icons-vue', () => ({
  ChatDotRound: { name: 'ChatDotRound' },
  Cpu: { name: 'Cpu' },
  Grid: { name: 'Grid' },
  Notebook: { name: 'Notebook' },
  Setting: { name: 'Setting' },
  Monitor: { name: 'Monitor' }
}));
// App.vue 的页面/导航/插件宿主等重依赖打成空壳，聚焦事件分支。
vi.mock('../../pages/MessagesPage.vue', () => ({ default: { name: 'MessagesPageStub', template: '<div />' } }));
vi.mock('../../pages/ContactsPage.vue', () => ({ default: { name: 'ContactsPageStub', template: '<div />' } }));
vi.mock('../../pages/AppsPage.vue', () => ({ default: { name: 'AppsPageStub', template: '<div />' } }));
vi.mock('../../pages/TestPage.vue', () => ({ default: { name: 'TestPageStub', template: '<div />' } }));
vi.mock('../../pages/SettingsPage.vue', () => ({ default: { name: 'SettingsPageStub', template: '<div />' } }));
vi.mock('../../pages/MinePage.vue', () => ({ default: { name: 'MinePageStub', template: '<div />' } }));
vi.mock('../../components/TopNavbar.vue', () => ({ default: { name: 'TopNavbarStub', template: '<div />' } }));
vi.mock('../../components/UserAvatarMenu.vue', () => ({ default: { name: 'UserAvatarMenuStub', template: '<div />' } }));
vi.mock('../../components/MobileTabBar.vue', () => ({ default: { name: 'MobileTabBarStub', template: '<div />' } }));
vi.mock('../../components/MobileTopBar.vue', () => ({ default: { name: 'MobileTopBarStub', template: '<div />' } }));
vi.mock('../../components/MobileSpaceDrawer.vue', () => ({ default: { name: 'MobileSpaceDrawerStub', template: '<div />' } }));
vi.mock('../../components/plugin/PluginIframeHost.vue', () => ({ default: { name: 'PluginIframeHostStub', template: '<div />' } }));
vi.mock('../../plugin/source', () => ({ fetchPluginManifest: vi.fn(async () => null) }));
vi.mock('../../stores/current-user', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../stores/current-user')>();
  return { ...actual, refreshCurrentUser: vi.fn(async () => {}) };
});

const captured = vi.hoisted(() => ({} as Record<string, (event: { payload: unknown }) => void>));

import ElementPlus, { ElMessage } from 'element-plus';
import App from '../../App.vue';
import { currentUser } from '../../stores/current-user';
import { pendingDeviceNotices, markDeviceNoticesSeen } from '../../stores/device-notices';

beforeEach(() => {
  localStorage.clear();
  markDeviceNoticesSeen('root-e2e');
  currentUser.rootId = 'root-e2e';
  Object.keys(captured).forEach((k) => delete captured[k]);
  vi.clearAllMocks();
  // App.vue setup 需要完整可调用的 electronAPI（system.exitApp 等），覆盖 test-setup 代理。
  (window as any).electronAPI = {
    system: { exitApp: vi.fn().mockResolvedValue(undefined) },
    rootIdentity: { status: vi.fn().mockResolvedValue({ initialized: true, unlocked: true, rootId: 'root-e2e', nickname: 'u', avatar: null }) },
    organization: { listMine: vi.fn().mockResolvedValue([]) },
    plugin: { listCatalog: vi.fn().mockResolvedValue([]) },
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

afterEach(() => {
  currentUser.rootId = null;
  pendingDeviceNotices.value = [];
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

function mountApp(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(App) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function dispatchP2pEvent(payload: unknown): void {
  const handler = captured['p2p-event'];
  expect(handler).toBeDefined();
  handler({ payload });
}

describe('App.vue DeviceNoticeReceived 新设备通知', () => {
  it('新设备通知 → ElMessage warning，文案含设备名与撤销提示', async () => {
    (ElMessage as unknown as ReturnType<typeof vi.fn>).mockClear();
    mountApp();
    await flush();

    dispatchP2pEvent({
      kind: 'DeviceNoticeReceived',
      data: { kind: 'device_joined', deviceId: 'peer-new-e2e', deviceName: '新平板', ts: 1700000000000 }
    });
    await flush();

    // 通知计入待看（红点数据源）。
    expect(pendingDeviceNotices.value).toHaveLength(1);
    expect(pendingDeviceNotices.value[0].deviceId).toBe('peer-new-e2e');
    expect(pendingDeviceNotices.value[0].deviceName).toBe('新平板');

    // ElMessage 对象式调用：type=warning，文案含设备名与撤销提示。
    const call = (ElMessage as unknown as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as {
      type: string;
      message: string;
    };
    expect(call.type).toBe('warning');
    expect(call.message).toContain('新平板');
    expect(call.message).toContain('如非本人操作请立即在设备管理中撤销');
  });

  it('App 主界面已注册 p2p-event listener（事件桥就绪）', async () => {
    mountApp();
    await flush();
    expect(captured['p2p-event']).toBeDefined();
  });
});
