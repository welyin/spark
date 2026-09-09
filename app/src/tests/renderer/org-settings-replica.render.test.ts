// OrgSettingsPanel 组织副本健康度（A41 核对 / product/todo #16，Q06「只告知、不干涉」）渲染回归：
// - 达标态：副本 3/3 + 「副本充足」，成员副本行无不足提醒；
// - 不达标态：副本 2/3 + 「副本不足」只提醒文案 + 成员副本行「合计不足 3 份」提醒，
//   手机设备如实标注（不计入 K）；
// - 无 K 组织（kApplicable=false，纯 all-members）：不做达标判定、不提醒，
//   只信息性呈现「全员持有」（membership §4.1 / batch2 §1.2）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import OrgSettingsPanel from '../../components/org/OrgSettingsPanel.vue';
import { switchToOrg, switchToPersonal } from '../../stores/current-space';
import type { OrgSyncOverviewDto } from '../../api/types';

const ORG_ID = 'org-test';

// 组织缓存由测试直接供给，不走 electronAPI.listMine
// （mock 工厂在模块初始化前执行，不能引用模块级变量，组织视图内联字面量）
vi.mock('../../stores/org-membership', () => ({
  refreshOrganizations: vi.fn().mockResolvedValue([]),
  findOrg: vi.fn((orgId: string) =>
    orgId === 'org-test'
      ? {
          orgId: 'org-test',
          name: '测试小区',
          description: '',
          createdAt: 1,
          createdBy: 'r-z',
          updatedAt: 1700000000000,
          members: [],
          currentUserRole: 'admin',
          isCurrentUserAdmin: true,
          memberCount: 3,
          adminCount: 1
        }
      : null
  )
}));

function overviewOf(patch: Partial<OrgSyncOverviewDto> = {}): OrgSyncOverviewDto {
  return {
    orgId: ORG_ID,
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
    memberReplicas: [
      { rootId: 'aaaa1111bbbb2222', pcSynced: true, deviceClass: 'pc' },
      { rootId: 'cccc3333dddd4444', pcSynced: true, deviceClass: 'pc' },
      { rootId: 'eeee5555ffff6666', pcSynced: true, deviceClass: 'pc' }
    ],
    ...patch
  };
}

// 面板含 Transition 覆盖层：只移除 DOM 不 unmount 会让后续响应式更新打到已移除
// 节点而抛错，故返回 app 由 afterEach 显式 unmount
function mount(): { host: HTMLElement; app: ReturnType<typeof createApp> } {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(OrgSettingsPanel) });
  app.use(ElementPlus);
  app.mount(host);
  return { host, app };
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('OrgSettingsPanel 组织副本健康度（只告知）', () => {
  let getSyncOverview: ReturnType<typeof vi.fn>;
  let host: HTMLElement;
  let app: ReturnType<typeof createApp>;

  function setupApi(overview: OrgSyncOverviewDto) {
    getSyncOverview = vi.fn().mockResolvedValue(overview);
    (window as any).electronAPI.organization = {
      getSyncOverview,
      getGatewayActiveSet: vi.fn().mockResolvedValue(['aaaa1111bbbb2222'])
    };
  }

  beforeEach(() => {
    switchToOrg(ORG_ID);
  });

  afterEach(() => {
    app?.unmount();
    host?.remove();
    switchToPersonal();
    vi.restoreAllMocks();
  });

  it('达标态：副本 3/3 副本充足，成员副本行无不足提醒', async () => {
    setupApi(overviewOf());
    ({ host, app } = mount());
    await flush();
    expect(host.textContent).toContain('副本 3/3');
    expect(host.textContent).toContain('副本充足');
    expect(host.textContent).not.toContain('副本不足');
    expect(host.textContent).toContain('成员副本');
    expect(host.textContent).not.toContain('成员 PC 副本合计不足');
  });

  it('不达标态：副本 2/3 如实提醒，只提醒文案无处置动作，手机如实标注', async () => {
    setupApi(
      overviewOf({
        syncedPeers: 2,
        memberReplicas: [
          { rootId: 'aaaa1111bbbb2222', pcSynced: true, deviceClass: 'pc' },
          { rootId: 'cccc3333dddd4444', pcSynced: true, deviceClass: 'pc' },
          { rootId: 'eeee5555ffff6666', pcSynced: false, deviceClass: 'mobile' }
        ]
      })
    );
    ({ host, app } = mount());
    await flush();
    expect(host.textContent).toContain('副本 2/3');
    expect(host.textContent).toContain('副本不足，建议成员保持在线或邀请更多节点');
    expect(host.textContent).toContain('成员 PC 副本合计不足 3 份，建议成员常备桌面端在线');
    // 手机叶子如实标注、不计入 K
    expect(host.textContent).toContain('（手机）');
    // 不达标只提醒不处置：两处提醒均为「建议…」句式，面板不提供任何副本处置按钮
    const replicaRows = Array.from(host.querySelectorAll('.replica-row'));
    expect(replicaRows.some((row) => row.textContent?.includes('副本不足'))).toBe(true);
    for (const row of replicaRows.filter((r) => r.textContent?.includes('副本'))) {
      expect(row.querySelector('button')).toBeNull();
    }
  });

  it('无 K 组织（纯 all-members）：不做达标判定、不提醒，信息性呈现全员持有', async () => {
    setupApi(
      overviewOf({
        syncedPeers: 1,
        totalMembers: 2,
        kApplicable: false,
        memberReplicas: []
      })
    );
    ({ host, app } = mount());
    await flush();
    expect(host.textContent).toContain('全员持有');
    expect(host.textContent).toContain('无副本目标');
    expect(host.textContent).not.toContain('副本不足');
    expect(host.textContent).not.toContain('副本充足');
    expect(host.textContent).not.toContain('成员副本');
  });
});
