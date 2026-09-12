/**
 * SpaceListPage（M24 一级域列表）渲染回归：
 * - 个人空间置顶（固定第一项，无删除/退出入口）；
 * - 组织项显示头像 / 组织名 / 域内昵称（未设置时回退「组织空间」）/ 待办角标；
 * - 搜索框按组织名与域内昵称过滤，无结果给独立文案。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import SpaceListPage from '../../components/SpaceListPage.vue';
import { organizations } from '../../stores/org-membership';
import { setOrgIdentity } from '../../stores/org-identity';
import { currentSpace, switchToPersonal } from '../../stores/current-space';
import type { OrgView } from '../../api/types';

function orgOf(orgId: string, name: string): OrgView {
  return {
    orgId,
    name,
    description: '',
    createdAt: 1,
    createdBy: 'root-x',
    updatedAt: 1,
    isCurrentUserAdmin: false,
    members: [{ rootId: 'root-me', role: 'member', joinedAt: 1, addedBy: 'root-x' }]
  } as OrgView;
}

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(SpaceListPage) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

beforeEach(() => {
  localStorage.clear();
  switchToPersonal();
  organizations.value = [];
  // 挂载时 refreshOrganizations：mock listMine 返回当前缓存（不覆盖测试预设）
  (window as any).electronAPI = {
    organization: { listMine: vi.fn().mockImplementation(async () => organizations.value) }
  };
});

afterEach(() => {
  document.body.innerHTML = '';
  void currentSpace;
  vi.restoreAllMocks();
});

describe('SpaceListPage（M24）', () => {
  it('个人空间置顶；组织项显示名称与域内昵称副标题', async () => {
    organizations.value = [orgOf('org-m24-a', '前端小组'), orgOf('org-m24-b', '读书会')];
    setOrgIdentity('org-m24-b', { nickname: '小王' });
    const host = mount();
    await flush();

    const items = host.querySelectorAll('.space-item');
    expect(items[0].textContent).toContain('个人空间');
    expect(items[1].textContent).toContain('前端小组');
    expect(items[1].textContent).toContain('组织空间');
    expect(items[2].textContent).toContain('读书会');
    expect(items[2].textContent).toContain('我在此域：小王');
  });

  it('搜索按组织名 / 域内昵称过滤；无结果给独立空态', async () => {
    organizations.value = [orgOf('org-m24-a', '前端小组'), orgOf('org-m24-b', '读书会')];
    setOrgIdentity('org-m24-b', { nickname: '小王' });
    const host = mount();
    await flush();

    const input = host.querySelector<HTMLInputElement>('.space-list-search input')!;
    expect(input).toBeTruthy();

    // 按昵称过滤
    input.value = '小王';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await flush();
    let names = Array.from(host.querySelectorAll('.space-item-name')).map((n) => n.textContent);
    expect(names).toEqual(['个人空间', '读书会']);

    // 无结果
    input.value = '不存在的空间';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await flush();
    names = Array.from(host.querySelectorAll('.space-item-name')).map((n) => n.textContent);
    expect(names).toEqual(['个人空间']);
    expect(host.textContent).toContain('未找到相关空间');
  });
});
