/**
 * MobileSettingsPage（移动端底栏第 5 Tab「设置」，M3/M9）挂载与导航测试：
 * 栈1=身份卡 + 个人设置｜系统设置｜测试｜退出登录；个人设置/测试切二级页（emit open-tab），
 * 系统设置压入本页栈2（SystemSettingsPanel 整页），退出登录走 identity-lock 收敛点。
 * 每个用例结束恢复桌面布局并清栈（matchMedia 桩见 test-setup.ts）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';

// 退出登录会整窗重载（jsdom 不支持 navigation）：打成桩，仅断言调用
vi.mock('../utils/identity-lock', () => ({ lockAndReload: vi.fn() }));

import MobileSettingsPage from './MobileSettingsPage.vue';
import { isMobileLayout } from '../stores/ui-layout';
import { canBack, currentPage, resetStack } from '../stores/mobile-nav';
import { requestOpenSystemSection } from '../stores/pending-system-section';
import { lockAndReload } from '../utils/identity-lock';

const mountPage = async () => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const openedTabs: string[] = [];
  const app = createApp(MobileSettingsPage, {
    onOpenTab: (id: string) => openedTabs.push(id)
  });
  const errors: unknown[] = [];
  app.config.errorHandler = (err) => errors.push(err);
  app.use(ElementPlus);
  app.mount(el);
  await nextTick();
  // 等 onMounted 里的异步 IPC（桩立即 resolve）引发的二次渲染
  await new Promise((resolve) => setTimeout(resolve, 30));
  await nextTick();
  return {
    el,
    errors,
    openedTabs,
    unmount: () => {
      app.unmount();
      el.remove();
    }
  };
};

/** 等栈帧滑动转场（260ms）结束：转场期间新旧两层同在 DOM（旧层为退场层） */
const settleTransition = async () => {
  await new Promise((resolve) => setTimeout(resolve, 340));
  await nextTick();
};

/** 按文字找按钮并点击 */
const clickItem = async (el: HTMLElement, text: string) => {
  const target = Array.from(el.querySelectorAll<HTMLElement>('button')).find((n) =>
    n.textContent?.trim().includes(text)
  );
  expect(target, `应存在按钮「${text}」`).toBeTruthy();
  target!.click();
  await nextTick();
};

describe('MobileSettingsPage 设置 Tab 壳页（M9）', () => {
  beforeEach(() => {
    isMobileLayout.value = true;
    resetStack('settings');
  });

  afterEach(() => {
    isMobileLayout.value = false;
    resetStack('settings');
  });

  it('栈1：身份卡 + 个人设置/系统设置/测试 + 置底退出登录（危险样式）', async () => {
    const { el, errors, unmount } = await mountPage();
    expect(errors.map(String)).toEqual([]);

    // 身份卡（头像 + 根身份昵称；test-setup 桩 rootIdentity 昵称「测试用户」经 currentUser 单例展示）
    expect(el.querySelector('.mobile-settings-identity')).toBeTruthy();

    const labels = Array.from(el.querySelectorAll('.mine-menu-label')).map((n) => n.textContent);
    expect(labels).toEqual(['个人设置', '系统设置', '测试', '退出登录']);
    // 退出登录为弱化危险样式按钮
    expect(el.querySelector('.mine-menu-item.mine-menu-danger')?.textContent).toContain('退出登录');
    expect(canBack('settings')).toBe(false);
    unmount();
  });

  it('个人设置/测试：emit open-tab 切二级页（MinePage/TestPage 由 App.vue 挂载）', async () => {
    const { el, openedTabs, unmount } = await mountPage();

    await clickItem(el, '个人设置');
    await clickItem(el, '测试');
    expect(openedTabs).toEqual(['mine', 'test']);
    // 本页栈不动（二级页切换由 App.vue 承担）
    expect(canBack('settings')).toBe(false);
    unmount();
  });

  it('系统设置：压入栈2 整页（返回栏 + SystemSettingsPanel），返回栏回设置列表', async () => {
    const { el, unmount } = await mountPage();

    await clickItem(el, '系统设置');
    await settleTransition();
    expect(currentPage('settings').page).toBe('system');
    expect(el.querySelector('.mobile-back-bar')).toBeTruthy();
    expect(el.querySelector('.mobile-back-title')?.textContent).toBe('系统设置');
    // 系统设置面板子菜单整页（移动端选中 section 前的列表）
    expect(el.querySelector('.system-settings-panel')).toBeTruthy();

    await clickItem(el, '返回');
    await settleTransition();
    expect(currentPage('settings').page).toBe('root');
    expect(el.querySelector('.mobile-back-bar')).toBeFalsy();
    unmount();
  });

  it('退出登录：走 identity-lock 收敛点（锁身份 + 整窗重载回登录门）', async () => {
    const { el, unmount } = await mountPage();
    await clickItem(el, '退出登录');
    expect(lockAndReload).toHaveBeenCalledWith('已退出登录');
    unmount();
  });

  it('网络状态点深链：挂载时消费 pending section，直达系统设置栈帧', async () => {
    requestOpenSystemSection('netStatus');
    const { el, unmount } = await mountPage();
    expect(currentPage('settings').page).toBe('system');
    expect(el.querySelector('.mobile-back-title')?.textContent).toBe('系统设置');
    unmount();
  });
});
