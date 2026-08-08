/**
 * 移动端整页 + 导航栈（波次 2）挂载测试：isMobileLayout 置 true 后，
 * 验证窄屏下同屏只渲染一层——点开详情整页切换、返回回上一栏；
 * 每个用例结束恢复桌面布局并清栈，不影响其他测试（matchMedia 桩见 test-setup.ts）。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApp, nextTick, type Component } from 'vue';
import ElementPlus from 'element-plus';
import { isMobileLayout } from '../stores/ui-layout';
import { canBack, pushPage, resetStack } from '../stores/mobile-nav';
import { ensureDirectConversation } from '../stores/messages';
import MessagesPage from './MessagesPage.vue';
import ContactsPage from './ContactsPage.vue';
import MinePage from './MinePage.vue';

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

const click = async (el: HTMLElement, selector: string) => {
  const target = el.querySelector<HTMLElement>(selector);
  expect(target, `应存在可点击元素 ${selector}`).toBeTruthy();
  target!.click();
  await nextTick();
};

/** 等栈帧滑动转场（260ms，app-shell.css 波次 3）结束：转场期间新旧两层同在 DOM（旧层为退场层） */
const settleTransition = async () => {
  await new Promise((resolve) => setTimeout(resolve, 340));
  await nextTick();
};

describe('移动端整页 + 导航栈（波次 2）', () => {
  beforeEach(() => {
    isMobileLayout.value = true;
    resetStack('messages');
    resetStack('contacts');
    resetStack('mine');
  });

  afterEach(() => {
    isMobileLayout.value = false;
    resetStack('messages');
    resetStack('contacts');
    resetStack('mine');
  });

  it('消息页：栈1 会话列表 → 点开会话整页（栈2）→ 聊天头 ‹ 返回列表', async () => {
    // 造一个 1:1 会话（测试桩无真实会话数据水合）
    ensureDirectConversation('personal', 'root-peer-mobile', '移动端好友');
    const { el, errors, unmount } = await mountPage(MessagesPage);
    expect(errors.map(String)).toEqual([]);

    // 栈1：仅会话列表整页，聊天区不渲染
    expect(el.querySelector('.conv-list')).toBeTruthy();
    expect(el.querySelector('.chat-view')).toBeFalsy();
    expect(canBack('messages')).toBe(false);

    // 点开会话 → 栈2：仅聊天页整页，列表不渲染（等滑动转场结束、退场层移除后断言）
    await click(el, '.conv-item');
    await settleTransition();
    expect(el.querySelector('.chat-view')).toBeTruthy();
    expect(el.querySelector('.conv-list')).toBeFalsy();
    expect(canBack('messages')).toBe(true);

    // 聊天头 ‹ 返回 → 回栈1 列表
    await click(el, '.chat-back');
    await settleTransition();
    expect(el.querySelector('.conv-list')).toBeTruthy();
    expect(el.querySelector('.chat-view')).toBeFalsy();
    expect(canBack('messages')).toBe(false);
    unmount();
  });

  it('消息页：重按当前 tab 复位回栈底（reset 不被拖窗补栈逻辑撤销）', async () => {
    ensureDirectConversation('personal', 'root-peer-mobile', '移动端好友');
    const { el, errors, unmount } = await mountPage(MessagesPage);
    expect(errors.map(String)).toEqual([]);

    // 点开会话 → 栈2 聊天页整页
    await click(el, '.conv-item');
    await settleTransition();
    expect(el.querySelector('.chat-view')).toBeTruthy();
    expect(canBack('messages')).toBe(true);

    // 模拟 App.vue 重按当前 tab：resetStack 回栈底，聊天帧不得被补栈逻辑压回
    resetStack('messages');
    await settleTransition();
    expect(el.querySelector('.conv-list')).toBeTruthy();
    expect(el.querySelector('.chat-view')).toBeFalsy();
    expect(canBack('messages')).toBe(false);
    unmount();
  });

  it('通讯录页：标签列表 → 成员管理整页 → 点成员开资料卡（导航栈逐层，不叠加抽屉）', async () => {
    const { el, errors, unmount } = await mountPage(ContactsPage);
    expect(errors.map(String)).toEqual([]);

    // 列表 → 标签列表整页（栈2；面板拆层 view="list"，本层只有标签行，无成员行）
    pushPage('contacts', 'tags');
    await settleTransition();
    expect(el.querySelector('.contacts-mobile-panel .request-item')).toBeTruthy();
    expect(el.querySelector('.tag-member-row')).toBeFalsy();

    // 点首个标签行 → 栈3 标签成员管理整页（种子首个标签「邻居」下有成员）
    await click(el, '.contacts-mobile-panel .request-item');
    await settleTransition();
    expect(el.querySelector('.tag-member-row')).toBeTruthy();

    // 点成员 → 栈4 资料卡整页；全文档只此一层 ContactPanel（移动端不再开抽屉叠加第二层）
    await click(el, '.tag-member-row');
    await settleTransition();
    expect(el.querySelector('.mobile-stack-layer .contact-panel')).toBeTruthy();
    expect(document.body.querySelectorAll('.contact-panel')).toHaveLength(1);
    unmount();
  });

  it('我的页：栈1 功能菜单 → 点开模块整页（栈2 带返回栏）→ 返回栏回菜单', async () => {
    const { el, errors, unmount } = await mountPage(MinePage);
    expect(errors.map(String)).toEqual([]);

    // 栈1：仅功能菜单整页（模块区与返回栏不渲染）
    expect(el.querySelector('.mine-menu')).toBeTruthy();
    expect(el.querySelector('.mine-list')).toBeFalsy();
    expect(el.querySelector('.mobile-back-bar')).toBeFalsy();

    // 点开「我的资料」→ 栈2：返回栏 + 模块整页，菜单不渲染（等滑动转场结束、退场层移除后断言）
    await click(el, '.mine-menu-item');
    await settleTransition();
    expect(el.querySelector('.mobile-back-bar')).toBeTruthy();
    expect(el.querySelector('.mobile-back-title')?.textContent).toBe('我的资料');
    expect(el.querySelector('.mine-menu')).toBeFalsy();
    expect(el.querySelector('.mine-list')).toBeTruthy();

    // 返回栏 ‹ 返回 → 回栈1 菜单
    await click(el, '.mobile-back-btn');
    await settleTransition();
    expect(el.querySelector('.mine-menu')).toBeTruthy();
    expect(el.querySelector('.mobile-back-bar')).toBeFalsy();
    expect(el.querySelector('.mine-list')).toBeFalsy();
    unmount();
  });
});
