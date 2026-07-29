/**
 * 主页面挂载冒烟测试：整树挂载（含 Element Plus），断言
 * 1) 渲染过程无错误抛到 errorHandler；
 * 2) 各页面关键结构（栏位容器）真实出现在 DOM 中。
 * 覆盖的是 build / tsc 都兜不住的一类问题：模板结构错误导致整页渲染为空
 * （如裸 <template> 吞掉子节点）。electronAPI/matchMedia 桩见 test-setup.ts。
 */
import { describe, it, expect } from 'vitest';
import { createApp, nextTick, type Component } from 'vue';
import ElementPlus from 'element-plus';
import App from '../App.vue';
import ContactsPage from './ContactsPage.vue';
import AppsPage from './AppsPage.vue';
import SettingsPage from './SettingsPage.vue';
import MinePage from './MinePage.vue';
import TestPage from './TestPage.vue';

const mountPage = async (component: Component) => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(component);
  const errors: unknown[] = [];
  app.config.errorHandler = (err) => errors.push(err);
  app.use(ElementPlus);
  app.mount(el);
  await nextTick();
  // 等 onMounted 里的异步 IPC（桩立即 resolve）引发的二次渲染
  await new Promise((resolve) => setTimeout(resolve, 30));
  await nextTick();
  const unmount = () => {
    app.unmount();
    el.remove();
  };
  return { el, errors, unmount };
};

describe('主页面挂载冒烟', () => {
  it('App 外壳：rail 导航 + 顶栏 + 默认消息页', async () => {
    const { el, errors, unmount } = await mountPage(App);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.rail')).toBeTruthy();
    expect(el.querySelector('.topbar')).toBeTruthy();
    expect(el.querySelector('.messages-page')).toBeTruthy();
    unmount();
  });

  it('通讯录页：左栏列表 + 右侧内容', async () => {
    const { el, errors, unmount } = await mountPage(ContactsPage);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.contacts-list')).toBeTruthy();
    unmount();
  });

  it('应用页：默认应用列表视图', async () => {
    const { el, errors, unmount } = await mountPage(AppsPage);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.apps-list')).toBeTruthy();
    unmount();
  });

  it('设置页：第二栏菜单 + 第三栏模块菜单', async () => {
    const { el, errors, unmount } = await mountPage(SettingsPage);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.mine-menu')).toBeTruthy();
    expect(el.querySelector('.mine-list')).toBeTruthy();
    unmount();
  });

  it('我的页：第二栏菜单 + 默认我的资料模块', async () => {
    const { el, errors, unmount } = await mountPage(MinePage);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.mine-menu')).toBeTruthy();
    unmount();
  });

  it('测试页：正常渲染', async () => {
    const { el, errors, unmount } = await mountPage(TestPage);
    expect(errors.map(String)).toEqual([]);
    expect(el.querySelector('.test-page')).toBeTruthy();
    unmount();
  });
});
