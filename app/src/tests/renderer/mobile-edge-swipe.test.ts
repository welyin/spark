/**
 * M14 屏幕左缘右滑返回（MobilePageTransition）：
 * - 手势起点 clientX ≤ 24px（左缘热区）+ 水平位移 ≥ 48px 且明显水平 → popPage 回上一帧；
 * - 起点不在左缘 / 位移不足 / 斜滑（非明显水平）→ 不返回；
 * - 栈深 1（一级页）无覆盖层时不跟踪手势（不 pop 栈底）。
 * jsdom 无 TouchEvent 构造器：普通 Event 补 touches 字段模拟（同 pull-refresh.test.ts）。
 */
import { afterEach, describe, expect, it } from 'vitest';
import { createApp } from 'vue';
import MobilePageTransition from '../../components/MobilePageTransition.vue';
import { currentPage, pushPage, resetStack } from '../../stores/mobile-nav';

const TAB = 'edge-swipe-test';

const mountStage = () => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    components: { MobilePageTransition },
    template: '<MobilePageTransition tab="' + TAB + '"><div class="stage-content">页</div></MobilePageTransition>'
  });
  app.mount(el);
  return { el, unmount: () => { app.unmount(); el.remove(); } };
};

const touch = (type: string, target: Element, clientX: number, clientY: number) => {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'touches', { value: [{ clientX, clientY }] });
  target.dispatchEvent(event);
};

/** 完整滑动手势：start → move → 松手 */
const swipe = (target: Element, fromX: number, fromY: number, toX: number, toY: number) => {
  touch('touchstart', target, fromX, fromY);
  touch('touchmove', target, toX, toY);
  touch('touchend', target, toX, toY);
};

afterEach(() => {
  resetStack(TAB);
});

describe('M14 左缘右滑返回', () => {
  it('左缘（x≤24）明显水平右滑 ≥48px：pop 回上一帧（域桌面 → 域列表同此链路）', () => {
    pushPage(TAB, 'detail', { id: 'x' });
    expect(currentPage(TAB).page).toBe('detail');
    const { el, unmount } = mountStage();
    const stage = el.querySelector('.mobile-stack-stage')!;

    swipe(stage, 10, 300, 100, 302);
    expect(currentPage(TAB).page).toBe('root');
    unmount();
  });

  it('手势起点不在左缘（x>24）：不返回', () => {
    pushPage(TAB, 'detail', { id: 'x' });
    const { el, unmount } = mountStage();
    const stage = el.querySelector('.mobile-stack-stage')!;

    swipe(stage, 100, 300, 220, 300);
    expect(currentPage(TAB).page).toBe('detail');
    unmount();
  });

  it('位移不足 48px / 斜滑（非明显水平）：不返回', () => {
    pushPage(TAB, 'detail', { id: 'x' });
    const { el, unmount } = mountStage();
    const stage = el.querySelector('.mobile-stack-stage')!;

    swipe(stage, 10, 300, 40, 300);
    expect(currentPage(TAB).page).toBe('detail');
    swipe(stage, 10, 300, 70, 350);
    expect(currentPage(TAB).page).toBe('detail');
    unmount();
  });

  it('栈深 1（一级页）：手势不接管（不退栈底）', () => {
    const { el, unmount } = mountStage();
    const stage = el.querySelector('.mobile-stack-stage')!;

    swipe(stage, 10, 300, 120, 300);
    expect(currentPage(TAB).page).toBe('root');
    unmount();
  });
});
