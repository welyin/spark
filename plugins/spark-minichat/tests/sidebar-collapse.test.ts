/**
 * 窄窗侧栏折叠交互的组件测试（插件窗口化适配）：
 * 挂载真实 MiniChatApp（假 SDK 内存会话/消息，两个会话），断言——
 * 1. 窄窗（≤560px，取 320 最小窗 / 480 窄窗）：侧栏带 --overlay 类、初始收起，
 *    ☰ 打开 → 遮罩出现；遮罩点击 / 选中会话均收起；
 * 2. 宽窗（880）：侧栏无 --overlay 类（常驻两栏），无 ☰ 按钮与遮罩；
 * 3. 宽窄切换复位抽屉开态；
 * 4. 重复壳检查：无「关闭应用/返回桌面」类壳元素。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApp, nextTick, type App } from 'vue';
import type { PluginContext, PluginSDK } from '../../../packages/plugin-sdk/src';
import MiniChatApp from '../src/MiniChatApp.vue';
import {
  activeConvId,
  activeMessages,
  bindMiniChatSdk,
  conversations
} from '../src/sdk-access';

const CONVS = [
  { id: 'dm:a', title: '会话A', unreadCount: 0 },
  { id: 'dm:b', title: '会话B', unreadCount: 2 }
];

const CTX: PluginContext = {
  pluginId: 'spark-minichat',
  viewId: 'default',
  domain: 'plugin:spark-minichat',
  space: { type: 'personal', id: 'personal' },
  theme: 'light',
  mount: { viewType: 'app' }
} as PluginContext;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 假 SDK：messages 四调用内存实现（验收样例最小面） */
function fakeSDK(): PluginSDK {
  return {
    messages: {
      conversations: async () =>
        CONVS.map((c) => ({
          kind: 'direct',
          peerId: c.id,
          pinnedAt: 0,
          muted: false,
          online: false,
          draft: '',
          updatedAt: 1_000,
          ...c
        })),
      list: async (convId: string) => [
        {
          id: `m-${convId}`,
          senderId: 'peer',
          senderName: '对方',
          type: 'text',
          content: `${convId} 的消息`,
          createdAt: 1_000,
          recalled: false
        }
      ],
      send: async () => ({}),
      markConversationRead: async () => ({}),
      onNewMessage: async () => {},
      onStatus: async () => {},
      onConversationsSynced: async () => {},
      onPeerPresence: async () => {}
    }
  } as unknown as PluginSDK;
}

let mounted: Array<{ app: App; host: HTMLElement }> = [];

/** 等 onMounted 链（refreshConversations/subscribeNewMessages）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mountMiniChat(width: number): Promise<HTMLElement> {
  setViewport(width);
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(MiniChatApp);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

const sidebar = (host: HTMLElement) => host.querySelector('.minichat-convs');
const backdrop = (host: HTMLElement) => host.querySelector('.minichat-backdrop');
const toggle = (host: HTMLElement) => host.querySelector<HTMLButtonElement>('.minichat-side-toggle');

describe('spark-minichat 窄窗侧栏折叠交互', () => {
  beforeEach(() => {
    bindMiniChatSdk(fakeSDK(), CTX);
  });

  afterEach(() => {
    for (const { app, host } of mounted) {
      app.unmount();
      host.remove();
    }
    mounted = [];
    // sdk-access 为模块级响应式缓存，用例间复位
    conversations.value = [];
    activeConvId.value = '';
    activeMessages.value = [];
  });

  it.each([320, 480])('窄窗（%i）：侧栏折叠收起，☰ 打开抽屉 + 遮罩，遮罩点击收起', async (width) => {
    const host = await mountMiniChat(width);

    expect(sidebar(host)?.classList.contains('minichat-convs--overlay')).toBe(true);
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(false);
    expect(backdrop(host)).toBeNull();

    const btn = toggle(host);
    expect(btn, '窄窗会话区应有 ☰ 入口').not.toBeNull();
    btn!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(true);
    expect(backdrop(host)).not.toBeNull();

    (backdrop(host) as HTMLElement).click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(false);
    expect(backdrop(host)).toBeNull();
  });

  it('窄窗：选中抽屉里的会话后自动收起并载入消息', async () => {
    const host = await mountMiniChat(480);

    toggle(host)!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(true);

    const items = host.querySelectorAll<HTMLElement>('.minichat-conv');
    expect(items.length).toBe(2);
    items[1].click();
    await flushMount();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(false);
    // 选中生效：消息区载入会话B的消息
    expect(activeConvId.value).toBe('dm:b');
    expect(host.querySelector('.minichat-msgs li')?.textContent).toContain('dm:b 的消息');
  });

  it('宽窗（880）：两栏常驻侧栏，无 ☰ 入口与遮罩', async () => {
    const host = await mountMiniChat(880);

    expect(sidebar(host)?.classList.contains('minichat-convs--overlay')).toBe(false);
    expect(toggle(host)).toBeNull();
    expect(backdrop(host)).toBeNull();
  });

  it('宽窄切换复位抽屉开态', async () => {
    const host = await mountMiniChat(480);

    toggle(host)!.click();
    await nextTick();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(true);

    setViewport(880);
    await nextTick();
    setViewport(480);
    await nextTick();
    expect(sidebar(host)?.classList.contains('minichat-convs--open')).toBe(false);
  });

  it('重复壳检查：无「关闭应用/返回桌面」类壳元素', async () => {
    const host = await mountMiniChat(480);
    const text = host.textContent ?? '';
    expect(text).not.toMatch(/关闭应用|返回桌面|退出登录/);
    expect(host.querySelector('[data-shell-chrome], .shell-close, .desktop-back')).toBeNull();
  });
});
