/**
 * NetworkStatusBar 当前空间口径回归（problem D3）：
 * - 桌面顶栏（variant="full"）胶囊 title 标明「当前空间网络状态」；
 * - 弹层顶部口径行「当前空间 · 空间名」随空间切换（个人空间 / 组织名），
 *   底部注明与左栏「我的」全局/设备维度状态的区分；
 * - 组织空间下数据源为 organization.getSyncOverview（当前空间/域维度），切空间时
 *   旧 overview 立即作废并按新组织重新拉取。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import NetworkStatusBar from '../../components/NetworkStatusBar.vue';
import { switchToOrg, switchToPersonal } from '../../stores/current-space';
import { organizations } from '../../stores/org-membership';
import type { OrgSyncOverviewDto, OrgView } from '../../api';

const orgView = (orgId: string, name: string): OrgView => ({
  orgId,
  name,
  description: '',
  createdAt: 1,
  createdBy: 'root-test',
  updatedAt: 1700000000000,
  members: [],
  currentUserRole: 'admin',
  isCurrentUserAdmin: true,
  memberCount: 2,
  adminCount: 1
} as OrgView);

const ORG_A = orgView('org-a', '测试小区A');
const ORG_B = orgView('org-b', '测试小区B');

const overviewOf = (orgId: string): OrgSyncOverviewDto => ({
  orgId,
  replicaTarget: 3,
  syncedPeers: 3,
  totalMembers: 3,
  members: [],
  connectedPeers: 2,
  recoveryState: 'idle',
  recoveryStartedAt: null,
  lastConnectedAt: null,
  dhtMode: 'client',
  status: 'good',
  kApplicable: true,
  memberReplicas: []
});

const mountBar = async () => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(NetworkStatusBar);
  app.use(ElementPlus);
  app.mount(host);
  await nextTick();
  return { host, app, unmount: () => { app.unmount(); host.remove(); } };
};

const flush = async () => {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
};

/** 悬停触发 popover，返回弹层根节点 */
const openPopover = async (host: HTMLElement) => {
  const trigger = host.querySelector('.net-status-tag');
  expect(trigger).toBeTruthy();
  trigger!.dispatchEvent(new Event('mouseenter', { bubbles: true }));
  await flush();
  return document.querySelector('.net-status-panel');
};

describe('NetworkStatusBar 当前空间口径（D3）', () => {
  let getSyncOverview: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    switchToPersonal();
    organizations.value = [ORG_A, ORG_B];
    getSyncOverview = vi.fn((orgId: string) => Promise.resolve(overviewOf(orgId)));
    (window as any).electronAPI.organization = {
      listMine: async () => [ORG_A, ORG_B],
      getSyncOverview
    };
  });

  afterEach(() => {
    switchToPersonal();
    organizations.value = [];
    document.body.querySelectorAll('.el-popper').forEach((n) => n.remove());
    vi.restoreAllMocks();
  });

  it('胶囊 title 标明「当前空间网络状态」', async () => {
    const { host, unmount } = await mountBar();
    const tag = host.querySelector('.net-status-tag');
    expect(tag?.getAttribute('title')).toContain('当前空间网络状态');
    unmount();
  });

  it('个人空间：弹层口径为「当前空间 · 个人空间」，并注明与全局/设备维度的区分', async () => {
    const { host, unmount } = await mountBar();
    const panel = await openPopover(host);
    expect(panel?.textContent).toContain('当前空间 · 个人空间');
    expect(panel?.textContent).toContain('全局 / 设备维度网络状态见左栏「我的」');
    unmount();
  });

  it('组织空间：数据源为当前组织 getSyncOverview，弹层口径随空间切换更新', async () => {
    switchToOrg('org-a');
    const { host, unmount } = await mountBar();
    await flush();
    expect(getSyncOverview).toHaveBeenCalledWith('org-a');

    const panel = await openPopover(host);
    expect(panel?.textContent).toContain('当前空间 · 测试小区A');

    // 切空间：按新组织重新拉取，弹层口径随之更新
    switchToOrg('org-b');
    await flush();
    expect(getSyncOverview).toHaveBeenCalledWith('org-b');
    const panel2 = await openPopover(host);
    expect(panel2?.textContent).toContain('当前空间 · 测试小区B');
    unmount();
  });
});
