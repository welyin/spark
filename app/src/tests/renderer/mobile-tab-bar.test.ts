/**
 * MobileTabBar 点击回归（移动端适配波次 1）：
 * 历史上 setup() 未从上下文解构 emit 导致模板中 emit 为 undefined，
 * 点击静默无效（真机「下导航栏点击不好使」）。本用例直接点击按钮断言事件。
 * M11 角标口径：消息=未读会话数（不是未读消息总数）、事务=「待我处理」数（affair-feed actionableCount）；
 * M13：再点当前 tab 时除 select 外另派发 spark:tab-reselect 事件（列表页据以回顶+刷新）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import MobileTabBar from '../../components/MobileTabBar.vue';
import { markRead, onChatReceived } from '../../stores/messages';
import { affairFeed, type AffairFeedItem } from '../../stores/affairs/affair-feed';

/** M13 重按当前 tab 事件名（与 MobileTabBar 内 TAB_RESELECT_EVENT 一致；.vue 具名导出不可被类型化消费，此处字面量对齐） */
const TAB_RESELECT_EVENT = 'spark:tab-reselect';

const mountBar = (activeTab: string, onSelect?: (id: string) => void) => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(MobileTabBar, {
    activeTab,
    onSelect: onSelect ?? (() => {})
  });
  app.use(ElementPlus);
  app.mount(el);
  return { el, unmount: () => { app.unmount(); el.remove(); } };
};

/** 向个人空间推一条未读会话（onChatReceived 纯内存路径，非 Tauri 环境） */
const pushUnreadConv = (peerId: string, unread: number): string => {
  const convId = `dm:${peerId}`;
  onChatReceived({
    spaceKey: 'personal',
    conversation: {
      id: convId,
      kind: 'direct',
      title: peerId,
      peerId,
      unreadCount: unread,
      pinnedAt: 0,
      muted: false,
      online: true,
      draft: '',
      updatedAt: Date.now()
    },
    message: {
      id: `msg-${peerId}`,
      senderId: peerId,
      senderName: peerId,
      type: 'text',
      content: '你好',
      createdAt: Date.now(),
      recalled: false
    }
  });
  return convId;
};

const affairItem = (affairId: string): AffairFeedItem => ({
  affairId,
  title: affairId,
  summary: '',
  tags: [],
  originator: 'root-x',
  createdAt: 1,
  closed: false,
  following: true,
  publicity: null,
  exec: null
});

afterEach(() => {
  affairFeed.value = [];
});

describe('MobileTabBar 点击切换', () => {
  it('点击五个 tab 均向外发出 select 事件并携带正确 id（M4/M9：消息·事务·空间·应用·设置）', () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const selected: string[] = [];
    const app = createApp(MobileTabBar, {
      activeTab: 'messages',
      onSelect: (id: string) => selected.push(id)
    });
    app.mount(el);

    const buttons = Array.from(el.querySelectorAll('button'));
    expect(buttons).toHaveLength(5);
    for (const button of buttons) {
      button.dispatchEvent(new Event('click', { bubbles: true }));
    }
    expect(selected).toEqual(['messages', 'affairs', 'space', 'apps', 'settings']);

    app.unmount();
    el.remove();
  });
});

describe('MobileTabBar 角标（M11）', () => {
  it('消息角标 = 未读会话数（两条会话各 3/5 条未读 → 角标 2，不是 8）', async () => {
    const convA = pushUnreadConv('m11-peer-a', 3);
    const convB = pushUnreadConv('m11-peer-b', 5);
    try {
      const { el, unmount } = mountBar('messages');
      await nextTick();
      const messagesButton = el.querySelectorAll('button')[0];
      expect(messagesButton.querySelector('.el-badge__content')?.textContent?.trim()).toBe('2');
      unmount();
    } finally {
      markRead('personal', convA);
      markRead('personal', convB);
    }
  });

  it('事务角标 = 「待我处理」数（进行中且我关注），非全部进行中数', async () => {
    affairFeed.value = [affairItem('affair-m11-a'), affairItem('affair-m11-b')];
    affairFeed.value[1].closed = true;
    const { el, unmount } = mountBar('affairs');
    await nextTick();
    const affairsButton = el.querySelectorAll('button')[1];
    expect(affairsButton.querySelector('.el-badge__content')?.textContent?.trim()).toBe('1');
    unmount();
  });
});

describe('MobileTabBar 重按当前 tab（M13）', () => {
  it('点已选中 tab：照常 emit select，并派发 spark:tab-reselect（detail=tab id）；点其它 tab 不派发', async () => {
    const selected: string[] = [];
    const reselected: string[] = [];
    const listener = (event: Event) => reselected.push((event as CustomEvent<string>).detail);
    window.addEventListener(TAB_RESELECT_EVENT, listener);
    try {
      const { el, unmount } = mountBar('messages', (id) => selected.push(id));
      const buttons = el.querySelectorAll('button');

      // 点当前 tab（消息）→ select + reselect
      buttons[0].dispatchEvent(new Event('click', { bubbles: true }));
      await nextTick();
      expect(selected).toEqual(['messages']);
      expect(reselected).toEqual(['messages']);

      // 点其它 tab（空间）→ 仅 select
      buttons[2].dispatchEvent(new Event('click', { bubbles: true }));
      await nextTick();
      expect(selected).toEqual(['messages', 'space']);
      expect(reselected).toEqual(['messages']);
      unmount();
    } finally {
      window.removeEventListener(TAB_RESELECT_EVENT, listener);
    }
  });
});
