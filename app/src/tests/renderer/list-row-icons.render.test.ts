// 渲染回归测试：Options API 组件在模板中直接使用图标组件时，必须经 components 注册——
// 仅 setup return 会导致 resolveComponent 失败（图标不渲染，只剩色块/空位）。
import { describe, expect, it } from 'vitest';
import { createApp, h, type Component } from 'vue';
import { ElBadge, ElIcon } from 'element-plus';
import GroupPanel from '../../components/contacts/GroupPanel.vue';
import MyCardModule from '../../components/mine/MyCardModule.vue';
import DevicesModule from '../../components/mine/DevicesModule.vue';

const P2P_STUB = {
  initialized: true,
  started: true,
  peerId: 'peer-stub',
  addresses: [],
  connectedPeers: [],
  sparkSyncSubscribers: []
};

function mount(target: Component, props: Record<string, unknown>): HTMLElement {
  const host = document.createElement('div');
  const app = createApp({ render: () => h(target, props) });
  app.component('el-icon', ElIcon);
  app.component('el-badge', ElBadge);
  app.mount(host);
  return host;
}

describe('列表行图标渲染（图标组件必须注册，不能仅 setup return）', () => {
  it('GroupPanel：功能行与虚拟组行图标渲染出 SVG', () => {
    const host = mount(GroupPanel, {
      mode: 'personal',
      spaceKey: 'personal',
      groups: [],
      counts: { ungrouped: 0 },
      pendingCount: 0,
      activeId: 'ungrouped'
    });
    // 新的朋友 + 标签 + 未分组 = 3 个行图标
    const icons = host.querySelectorAll('.row-icon svg');
    expect(icons.length).toBe(3);
  });

  it('MyCardModule：名片方式行图标渲染出 SVG', () => {
    const host = mount(MyCardModule, {});
    const icons = host.querySelectorAll('.mine-list-item-icon svg');
    expect(icons.length).toBe(2);
  });

  it('DevicesModule：设备行图标渲染出 SVG', async () => {
    // 设备清单来自内核 devices.list（多设备同步改造后不再渲染 p2pInfo 占位行），
    // 测试环境 stub 一条本机设备记录
    (window as unknown as { electronAPI: unknown }).electronAPI = {
      devices: {
        list: async () => [
          {
            peerId: 'peer-stub',
            deviceName: '测试机',
            os: 'Android',
            arch: 'aarch64',
            macs: [],
            updatedAt: 1,
            lastSeenAt: 1,
            isSelf: true,
            online: true
          }
        ]
      }
    };
    const host = mount(DevicesModule, { rootId: '', p2pInfo: P2P_STUB });
    // onMounted 的 load() 是异步的：等宏任务让清单落位
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(host.querySelector('.mine-list-item-icon svg')).not.toBeNull();
  });
});
