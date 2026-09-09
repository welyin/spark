/**
 * 窄窗侧栏折叠交互的组件测试（插件窗口化适配）：
 * 挂载真实 ChatView（假 SDK 内存 docs，两个 Bot），断言——
 * 1. 窄窗（≤600px，取默认窗 480）：侧栏带 --overlay 类、初始收起，
 *    ☰ 打开 → 遮罩出现；遮罩点击 / 选中 Bot 均收起；
 * 2. 宽窗（880）：侧栏无 --overlay 类（常驻两栏），无 ☰ 按钮与遮罩；
 * 3. 宽窄切换复位抽屉开态。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApp, nextTick, type App } from 'vue';
import type { PluginSDK } from '../../../packages/plugin-sdk/src/index';
import ChatView from '../ChatView.vue';

const BOTS = [
  { id: 'bot-a', name: 'Alpha', backendType: 'openai' },
  { id: 'bot-b', name: 'Beta', backendType: 'ollama' },
];

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 假 SDK：docs 内存实现，query bots 集合返回两个 Bot，其余集合空 */
function fakeSDK(): PluginSDK {
  const docs = {
    defineCollection: async () => ({}),
    query: async (collection: string) => ({
      items: collection === 'ai_chat_bots'
        ? BOTS.map((b) => ({ id: b.id, data: { ...b } }))
        : [],
    }),
    get: async () => null,
    put: async () => ({}),
    delete: async () => ({}),
  };
  return { docs } as unknown as PluginSDK;
}

let mounted: Array<{ app: App; host: HTMLElement }> = [];

/** 等 onMounted 链（ensurePluginSDK → loadBots → 默认选中首个 Bot → 历史加载）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mountChatView(width: number): Promise<HTMLElement> {
  setViewport(width);
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(ChatView);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

const sidebar = (host: HTMLElement) => host.querySelector('.sidebar');
const backdrop = (host: HTMLElement) => host.querySelector('.sidebar-backdrop');
const toggle = (host: HTMLElement) => host.querySelector<HTMLButtonElement>('.btn-sidebar-toggle');

describe('ai-chat 窄窗侧栏折叠交互', () => {
  beforeEach(() => {
    window.__sparkPluginSDK = fakeSDK();
  });

  afterEach(() => {
    for (const { app, host } of mounted) {
      app.unmount();
      host.remove();
    }
    mounted = [];
    delete window.__sparkPluginSDK;
  });

  it('窄窗（480）：侧栏折叠收起，☰ 打开抽屉 + 遮罩，遮罩点击收起', async () => {
    const host = await mountChatView(480);

    expect(sidebar(host)?.classList.contains('sidebar--overlay')).toBe(true);
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(false);
    expect(backdrop(host)).toBeNull();

    const btn = toggle(host);
    expect(btn, '窄窗聊天头部应有 ☰ 入口').not.toBeNull();
    btn!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(true);
    expect(backdrop(host)).not.toBeNull();

    (backdrop(host) as HTMLElement).click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(false);
    expect(backdrop(host)).toBeNull();
  });

  it('窄窗：选中抽屉里的 Bot 后自动收起', async () => {
    const host = await mountChatView(480);

    toggle(host)!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(true);

    const items = host.querySelectorAll<HTMLElement>('.bot-item');
    expect(items.length).toBe(2);
    items[1].click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(false);
    // 选中生效：聊天头部切到 Beta
    expect(host.querySelector('.chat-header-info strong')?.textContent).toBe('Beta');
  });

  it('宽窗（880）：两栏常驻侧栏，无 ☰ 入口与遮罩', async () => {
    const host = await mountChatView(880);

    expect(sidebar(host)?.classList.contains('sidebar--overlay')).toBe(false);
    expect(toggle(host)).toBeNull();
    expect(backdrop(host)).toBeNull();
  });

  it('宽窄切换复位抽屉开态', async () => {
    const host = await mountChatView(480);

    toggle(host)!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(true);

    setViewport(880);
    await nextTick();
    setViewport(480);
    await nextTick();
    expect(sidebar(host)?.classList.contains('sidebar--open')).toBe(false);
  });
});
