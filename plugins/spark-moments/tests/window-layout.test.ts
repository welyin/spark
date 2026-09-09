/**
 * 窗口化适配断言（PC 插件窗口化，ui-architecture §4.2；窗口最小夹取 320×220）。
 *
 * 时间线为单列流式布局、无宽度断点（max-width 640 居中自适应），320/480/880
 * 三档同构——断言聚焦结构不变量而非像素（jsdom 不做排版）：
 * 1. 三档视口下主页顶栏/发表入口/九宫格/作曲器工具栏结构完整、无壳层重复元素；
 * 2. 「退出朋友圈」按钮（back-fab → sdk.close）按形态显隐：
 *    PC 窗口（fine 指针）隐藏——WindowFrame 自带关闭钮，插件内退出钮语义重复；
 *    移动全屏（coarse 指针）保留——壳层沉浸式无可见返回，此钮是唯一可见出口；
 * 3. pointer 形态切换时按钮显隐即时联动。
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick, type App, type Component } from 'vue';
import ElementPlus from 'element-plus';
import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import type { isTouchLayout as IsTouchLayout } from '../ui-layout';
import type { MomentsImage } from '../model';

// ---------------------------------------------------------------------------
// matchMedia mock：jsdom 无实现；ui-layout 模块加载前必须就位（动态 import）
// ---------------------------------------------------------------------------

type ChangeListener = (event: { matches: boolean }) => void;
const coarseListeners = new Set<ChangeListener>();
const coarseMql = {
  matches: false,
  media: '(pointer: coarse)',
  onchange: null,
  addEventListener: (_type: string, listener: ChangeListener) => coarseListeners.add(listener),
  removeEventListener: (_type: string, listener: ChangeListener) => coarseListeners.delete(listener),
  addListener: (listener: ChangeListener) => coarseListeners.add(listener),
  removeListener: (listener: ChangeListener) => coarseListeners.delete(listener),
  dispatchEvent: () => true
};

/** 切换触屏形态并派发 change（模拟窗口壳形态变化） */
function setCoarse(matches: boolean): void {
  coarseMql.matches = matches;
  for (const listener of [...coarseListeners]) {
    listener({ matches });
  }
}

function installMatchMediaMock(): void {
  window.matchMedia = ((query: string) =>
    query === '(pointer: coarse)' ? coarseMql : { ...coarseMql, media: query, matches: false }) as unknown as typeof window.matchMedia;
}

// ---------------------------------------------------------------------------
// mock SDK：覆盖 MomentsView 初始化链路（runtime/data/feed/contacts/identity）+ close
// ---------------------------------------------------------------------------

function createMockSdk() {
  return {
    domain: 'plugin:spark-moments',
    runtime: {
      currentRoot: vi.fn().mockResolvedValue({ rootId: 'root-me', unlocked: true, nickname: '我', avatar: null })
    },
    data: {
      declareCollection: vi.fn().mockResolvedValue({}),
      save: vi.fn().mockResolvedValue({ success: true }),
      get: vi.fn().mockResolvedValue(null),
      query: vi.fn().mockResolvedValue({ items: [], nextCursor: undefined }),
      saveBlob: vi.fn().mockResolvedValue({ hash: 'h', size: 1 }),
      readBlob: vi.fn().mockResolvedValue({ status: 'pending' }),
      delete: vi.fn().mockResolvedValue({})
    },
    feed: {
      deliver: vi.fn().mockResolvedValue({ requested: 0, accepted: 0 }),
      onReceive: vi.fn().mockResolvedValue({}),
      pull: vi.fn().mockResolvedValue({ items: [], nextCursor: undefined })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({ signature: 'sig', publicKey: 'pk', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    contacts: {
      listFriends: vi.fn().mockResolvedValue([]),
      listGroups: vi.fn().mockResolvedValue([]),
      listTags: vi.fn().mockResolvedValue([])
    },
    messages: { sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }) },
    close: vi.fn().mockResolvedValue(undefined)
  } as unknown as PluginSDK & { close: ReturnType<typeof vi.fn> };
}

// ---------------------------------------------------------------------------
// 挂载辅助
// ---------------------------------------------------------------------------

let mounted: Array<{ app: App; host: HTMLElement }> = [];
let sdk: ReturnType<typeof createMockSdk>;
let touchLayout: typeof IsTouchLayout;
let MomentsView: Component;
let NineGrid: Component;
let ComposerView: Component;

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

/** 等 onMounted 异步链（init → ready）渲染完成 */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
  }
}

async function mount(component: Component, props: Record<string, unknown> = {}, withElementPlus = false): Promise<HTMLElement> {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp(component, props);
  if (withElementPlus) app.use(ElementPlus);
  app.mount(host);
  mounted.push({ app, host });
  await flushMount();
  return host;
}

const images = (count: number): MomentsImage[] =>
  Array.from({ length: count }, (_, i) => ({
    hash: `h${i}`,
    thumbHash: `t${i}`,
    name: `p${i}.jpg`,
    size: 100,
    mime: 'image/jpeg'
  }));

beforeAll(async () => {
  installMatchMediaMock();
  sdk = createMockSdk();
  (window as unknown as { __sparkPluginSDK: PluginSDK }).__sparkPluginSDK = sdk;
  // matchMedia mock 与 SDK 注入后再加载被测模块（ui-layout 在模块加载时读 pointer 形态）
  ({ isTouchLayout: touchLayout } = await import('../ui-layout'));
  MomentsView = (await import('../MomentsView.vue')).default;
  NineGrid = (await import('../components/NineGrid.vue')).default;
  ComposerView = (await import('../composer/ComposerView.vue')).default;
});

beforeEach(() => {
  setCoarse(false); // 默认 PC 窗口形态（fine 指针）
});

afterEach(() => {
  for (const { app, host } of mounted) {
    app.unmount();
    host.remove();
  }
  mounted = [];
});

describe('spark-moments 窗口化布局三档断言', () => {
  it.each([320, 480, 880])('窗口宽 %i：主页顶栏/标题/发表入口完整，无退出钮与壳层重复元素', async (width) => {
    setViewport(width);
    const host = await mount(MomentsView, {}, true);
    expect(host.querySelector('.moments-topbar')).not.toBeNull();
    expect(host.querySelector('.cover-title')?.textContent).toBe('朋友圈');
    expect(host.querySelector('.compose-fab')).not.toBeNull();
    // PC 窗口（fine 指针）：插件内退出钮隐藏（WindowFrame 自带关闭钮）
    expect(host.querySelector('.back-fab')).toBeNull();
    // 重复壳检查：无「关闭应用/返回桌面/退出」类壳元素
    expect(host.textContent ?? '').not.toMatch(/关闭应用|返回桌面|退出/);
  });

  it.each([320, 480, 880])('窗口宽 %i：九宫格列数结构正确（1 单图 / 4 两列 / 9 三列）', async (width) => {
    setViewport(width);
    const single = await mount(NineGrid, { images: images(1) });
    expect(single.querySelector('.nine-grid')?.classList.contains('grid-1')).toBe(true);
    expect(single.querySelectorAll('.cell.single')).toHaveLength(1);

    const four = await mount(NineGrid, { images: images(4) });
    expect(four.querySelector('.nine-grid')?.classList.contains('grid-4')).toBe(true);
    expect(four.querySelectorAll('.cell')).toHaveLength(4);

    const nine = await mount(NineGrid, { images: images(9) });
    expect(nine.querySelector('.nine-grid')?.classList.contains('grid-9')).toBe(true);
    expect(nine.querySelectorAll('.cell')).toHaveLength(9);
  });

  it.each([320, 480, 880])('窗口宽 %i：作曲器工具栏（取消/发表）与图片添加入口完整', async (width) => {
    setViewport(width);
    const host = await mount(ComposerView);
    expect(host.querySelector('.composer-top .cancel')?.textContent).toBe('取消');
    expect(host.querySelector('.composer-top .publish')?.textContent).toContain('发表');
    expect(host.querySelector('.image-grid .add-cell')).not.toBeNull();
    expect(host.querySelector('.visibility-row')).not.toBeNull();
  });
});

describe('spark-moments 「退出朋友圈」按钮形态显隐', () => {
  it('移动全屏（coarse 指针）：退出钮渲染，点击调 sdk.close 退出插件', async () => {
    setCoarse(true);
    expect(touchLayout.value).toBe(true);
    const host = await mount(MomentsView, {}, true);
    const backFab = host.querySelector<HTMLButtonElement>('.back-fab');
    expect(backFab).not.toBeNull();
    sdk.close.mockClear();
    backFab!.click();
    await flushMount();
    expect(sdk.close).toHaveBeenCalledTimes(1);
  });

  it('pointer 形态切换：coarse→fine 退出钮消失，fine→coarse 复现', async () => {
    setCoarse(true);
    const host = await mount(MomentsView, {}, true);
    expect(host.querySelector('.back-fab')).not.toBeNull();

    setCoarse(false);
    await nextTick();
    expect(host.querySelector('.back-fab')).toBeNull();

    setCoarse(true);
    await nextTick();
    expect(host.querySelector('.back-fab')).not.toBeNull();
  });
});
