/**
 * SpaceDesktop（M27 最近应用卡片栈 / M30 长按编辑模式）渲染回归：
 * - M30：长按图标进编辑模式（抖动+完成条）；点 × 移除快捷方式（localStorage 持久化、
 *   应用不卸载），已移除区可恢复；「添加应用」瓦片报 open-market；
 * - M27：双击 Home 指示条调出最近应用卡片栈（数据源 spark:apps-recent:<spaceId>），
 *   点卡片报 open-app（回存活实例由 App.vue openPluginTab 复用），× 关闭并移出最近列表。
 * jsdom 无 TouchEvent：普通 Event 补 touches 字段模拟（同 pull-refresh.test.ts）。
 * 长按用真实计时器等 600ms（fake timers 会卡死挂载时的异步刷新）；
 * 面板关闭断言等 400ms（Vue Transition 离场动画时长）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import SpaceDesktop from '../../components/SpaceDesktop.vue';
import { switchToPersonal } from '../../stores/current-space';
import type { PluginMarketItemDto } from '../../api/types';

const appItem = (id: string, name: string): PluginMarketItemDto =>
  ({
    id,
    domain: `plugin:${id}`,
    name,
    description: '',
    icon: '',
    category: 'tool',
    version: '1.0.0',
    views: ['default'],
    permissions: [],
    supportedSpaces: ['personal', 'org'],
    package: { updateManifestUrl: '', signatureUrl: '', packageName: '', installCommand: '' },
    installed: true,
    enabled: true,
    installedVersion: '1.0.0',
    latestVersion: '1.0.0',
    updateAvailable: false,
    lastCheckedAt: null,
    lastCheckReason: '',
    grantedPermissions: []
  }) as PluginMarketItemDto;

const mount = (emitted: Record<string, unknown[]>) => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({
    render: () =>
      h(SpaceDesktop, {
        onBack: () => { emitted.back = []; },
        onOpenMarket: () => { emitted['open-market'] = []; },
        onOpenApp: (payload: unknown) => { (emitted['open-app'] ??= []).push(payload); }
      })
  });
  app.use(ElementPlus);
  app.mount(host);
  return host;
};

const flush = async (ms = 0) => {
  await new Promise((resolve) => setTimeout(resolve, ms));
  await nextTick();
};

const touch = (type: string, target: Element, clientX: number, clientY: number) => {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'touches', { value: [{ clientX, clientY }] });
  target.dispatchEvent(event);
};

/** 双击 Home 指示条调出最近应用面板 */
const openRecents = async (host: HTMLElement) => {
  const indicator = host.querySelector('.home-indicator')!;
  indicator.dispatchEvent(new Event('click', { bubbles: true }));
  indicator.dispatchEvent(new Event('click', { bubbles: true }));
  await flush();
};

beforeEach(() => {
  localStorage.clear();
  switchToPersonal();
  (window as any).electronAPI = {
    pluginMarket: {
      list: vi.fn().mockResolvedValue([appItem('vote', '投票'), appItem('wiki', '文档')])
    }
  };
});

afterEach(() => {
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

describe('SpaceDesktop 编辑模式（M30）', () => {
  it('长按图标进编辑模式；× 移除快捷方式（持久化、不卸载）；已移除区可恢复', async () => {
    const emitted: Record<string, unknown[]> = {};
    const host = mount(emitted);
    await flush();

    // 正常态：点图标打开应用
    let icons = host.querySelectorAll<HTMLElement>('.app-icon');
    expect(icons).toHaveLength(2);
    icons[0].click();
    expect(emitted['open-app']).toHaveLength(1);

    // 长按进编辑模式
    const icon = host.querySelector('.app-icon')!;
    touch('touchstart', icon, 100, 100);
    await flush(600);
    touch('touchend', icon, 100, 100);
    await flush();
    expect(host.querySelector('.desktop-edit-done')).toBeTruthy();

    // 编辑态点图标不再打开应用
    emitted['open-app'] = [];
    host.querySelector<HTMLElement>('.app-icon')!.click();
    expect(emitted['open-app']).toHaveLength(0);

    // × 移除快捷方式：进入已移除区，localStorage 落盘
    host.querySelector<HTMLElement>('.app-icon-remove')!.click();
    await flush();
    expect(JSON.parse(localStorage.getItem('spark:desktop-hidden:personal') ?? '[]')).toEqual(['vote']);
    expect(host.querySelector('.desktop-hidden-title')?.textContent).toContain('已移除的快捷方式');

    // 恢复
    host.querySelector<HTMLElement>('.app-icon-restore')!.click();
    await flush();
    expect(JSON.parse(localStorage.getItem('spark:desktop-hidden:personal') ?? '[]')).toEqual([]);

    // 添加应用瓦片 → open-market；完成退出编辑
    const addTile = host.querySelector<HTMLElement>('.app-icon-add');
    expect(addTile).toBeTruthy();
    addTile!.click();
    expect(emitted['open-market']).toBeTruthy();
    host.querySelector<HTMLElement>('.desktop-edit-done')!.click();
    await flush();
    icons = host.querySelectorAll<HTMLElement>('.app-icon');
    expect(icons).toHaveLength(2);
  });
});

describe('SpaceDesktop 最近应用卡片栈（M27 简版）', () => {
  it('双击 Home 指示条调出；点卡片报 open-app；× 关闭实例并移出最近列表', async () => {
    localStorage.setItem('spark:apps-recent:personal', JSON.stringify(['vote']));
    const emitted: Record<string, unknown[]> = {};
    const closeEvents: string[] = [];
    const onClosePlugin = (event: Event) => {
      closeEvents.push((event as CustomEvent<{ pluginDomain: string }>).detail.pluginDomain);
    };
    window.addEventListener('spark:close-plugin', onClosePlugin);

    try {
      const host = mount(emitted);
      await flush();

      await openRecents(host);
      const panel = document.querySelector('.recents-panel');
      expect(panel).toBeTruthy();
      expect(panel!.textContent).toContain('投票');

      // 点卡片 → open-app（App.vue 复用存活 tab）
      panel!.querySelector<HTMLElement>('.recents-card-main')!.click();
      await flush();
      expect(emitted['open-app']).toHaveLength(1);
      expect((emitted['open-app'][0] as { pluginDomain: string }).pluginDomain).toBe('plugin:vote');
      // 面板关闭（留 400ms 给离场过渡）
      await flush(400);
      expect(document.querySelector('.recents-panel')).toBeNull();

      // 再调出：× 关闭 → spark:close-plugin + 移出最近
      await openRecents(host);
      document.querySelector<HTMLElement>('.recents-card-close')!.click();
      await flush();
      expect(closeEvents).toEqual(['plugin:vote']);
      expect(JSON.parse(localStorage.getItem('spark:apps-recent:personal') ?? '[]')).toEqual([]);
      expect(document.querySelector('.recents-panel')?.textContent).toContain('暂无最近使用的应用');
    } finally {
      window.removeEventListener('spark:close-plugin', onClosePlugin);
    }
  });
});
