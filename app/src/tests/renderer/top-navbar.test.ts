/**
 * TopNavbar 顶栏上下文菜单回归（problem D1 / D2）：
 * - D1：「当前空间」点击下弹菜单，含 切换空间（个人空间 + 已加入组织，点击即切换）/
 *   创建空间（开 L10 创建组织对话框）/ 空间设置（个人空间开名字/logo 设置对话框，
 *   组织空间开 OrgSettingsPanel 对话框）；
 * - D2：「当前身份」点击下弹菜单含身份设置——个人空间走 shell-modal「我的」（MinePage，
 *   根身份个人设置）；组织空间开组织身份对话框（OrgIdentityDialog，域内身份＋成员权限）。
 * 组织缓存直接写入 org-membership store，不走 electronAPI.listMine。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import TopNavbar from '../../components/TopNavbar.vue';
import { currentSpace, switchToOrg, switchToPersonal } from '../../stores/current-space';
import { organizations } from '../../stores/org-membership';
import { membershipDialogMode } from '../../stores/org-membership-dialog';
import { activeShellModal } from '../../stores/shell-modal';
import type { OrgView } from '../../api';

const ORG: OrgView = {
  orgId: 'org-a',
  name: '测试小区A',
  description: '',
  createdAt: 1,
  createdBy: 'root-test',
  updatedAt: 1700000000000,
  members: [],
  currentUserRole: 'admin',
  isCurrentUserAdmin: true,
  memberCount: 2,
  adminCount: 1
} as OrgView;

const mountNavbar = async () => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(TopNavbar);
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

/** 点击触发器并等 popper 落到 body，返回菜单项文本列表 */
const openDropdown = async (host: HTMLElement, selector: string) => {
  const trigger = host.querySelector(selector);
  expect(trigger).toBeTruthy();
  trigger!.dispatchEvent(new Event('click', { bubbles: true }));
  await flush();
  return Array.from(document.querySelectorAll('.el-dropdown-menu__item'));
};

const clickItem = async (items: Element[], text: string) => {
  const item = items.find((n) => n.textContent?.includes(text));
  expect(item, `菜单项「${text}」应存在`).toBeTruthy();
  item!.dispatchEvent(new Event('click', { bubbles: true }));
  await flush();
};

describe('TopNavbar 顶栏上下文菜单（D1/D2）', () => {
  beforeEach(() => {
    switchToPersonal();
    organizations.value = [ORG];
    // 空间菜单打开时会 refreshOrganizations：让 listMine 返回同一列表，避免刷新清空缓存
    (window as any).electronAPI.organization.listMine = async () => [ORG];
  });

  afterEach(() => {
    switchToPersonal();
    organizations.value = [];
    membershipDialogMode.value = null;
    activeShellModal.value = null;
    // el-dropdown popper 与 el-dialog 挂在 body，逐个用例后清理避免污染下一用例
    document.body.querySelectorAll('.el-popper, .el-overlay').forEach((n) => n.remove());
  });

  it('D1：空间菜单含 切换空间组 / 创建空间 / 空间设置，个人空间下列出个人空间与组织', async () => {
    const { host, unmount } = await mountNavbar();
    const items = await openDropdown(host, '.context-space');
    const texts = items.map((n) => n.textContent ?? '');
    expect(texts.some((t) => t.includes('个人空间'))).toBe(true);
    expect(texts.some((t) => t.includes('测试小区A'))).toBe(true);
    expect(texts.some((t) => t.includes('创建空间'))).toBe(true);
    expect(texts.some((t) => t.includes('空间设置'))).toBe(true);
    unmount();
  });

  it('D1：个人空间下「空间设置」打开个人空间设置对话框（名字/logo）；组织空间打开 OrgSettingsPanel 对话框', async () => {
    const { host, unmount } = await mountNavbar();
    const items = await openDropdown(host, '.context-space');
    const settingsItem = items.find((n) => n.textContent?.includes('空间设置'));
    expect(settingsItem!.classList.contains('is-disabled')).toBe(false);
    settingsItem!.dispatchEvent(new Event('click', { bubbles: true }));
    await flush();
    expect(document.body.textContent).toContain('空间设置 · 个人空间');

    switchToOrg('org-a');
    await flush();
    const items2 = await openDropdown(host, '.context-space');
    const settingsItem2 = items2.find((n) => n.textContent?.includes('空间设置'));
    expect(settingsItem2!.classList.contains('is-disabled')).toBe(false);
    settingsItem2!.dispatchEvent(new Event('click', { bubbles: true }));
    await flush();
    expect(document.body.textContent).toContain('空间设置 · 测试小区A');
    unmount();
  });

  it('D1：切换空间项点击即 switchSpace（组织 ↔ 个人空间）', async () => {
    const { host, unmount } = await mountNavbar();
    const items = await openDropdown(host, '.context-space');
    await clickItem(items, '测试小区A');
    expect(currentSpace.value).toEqual({ type: 'org', orgId: 'org-a' });

    const items2 = await openDropdown(host, '.context-space');
    await clickItem(items2, '个人空间');
    expect(currentSpace.value).toEqual({ type: 'personal' });
    unmount();
  });

  it('D1：「创建空间」打开创建组织对话框（org-membership-dialog）', async () => {
    const { host, unmount } = await mountNavbar();
    const items = await openDropdown(host, '.context-space');
    await clickItem(items, '创建空间');
    expect(membershipDialogMode.value).toBe('create');
    unmount();
  });

  it('D2：身份菜单含身份设置，点击打开壳层「我的」对话框', async () => {
    const { host, unmount } = await mountNavbar();
    const items = await openDropdown(host, '.context-identity');
    expect(items.some((n) => n.textContent?.includes('身份设置'))).toBe(true);
    await clickItem(items, '身份设置');
    expect(activeShellModal.value).toBe('mine');
    unmount();
  });
});
