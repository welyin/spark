/**
 * PullRefresh 下拉刷新容器（M8，docs/ui/problem.md）手势测试：
 * 列表滚到顶部时下拉过阈值触发刷新（带「正在刷新」指示），未过阈值回弹不触发，
 * 内部滚动未回顶部或上滑时不接管手势（不与列表滚动冲突）。
 * jsdom 无 TouchEvent 构造器：用普通 Event 补 touches 字段模拟触摸序列。
 */
import { describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import PullRefresh from '../../components/PullRefresh.vue';

const mountPullRefresh = (onRefresh: () => void | Promise<void>) => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    components: { PullRefresh },
    setup: () => ({ onRefresh }),
    template:
      '<PullRefresh :on-refresh="onRefresh">' +
      '<div class="inner-scroll"><span class="inner-item">item</span></div>' +
      '</PullRefresh>'
  });
  app.use(ElementPlus);
  app.mount(el);
  return { el, unmount: () => { app.unmount(); el.remove(); } };
};

/** 模拟一个触摸事件（jsdom 无 Touch/TouchEvent）：补 touches 字段后派发 */
const touch = (type: string, target: Element, clientY: number) => {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'touches', { value: [{ clientY }] });
  target.dispatchEvent(event);
};

/** 完整下拉手势：startY → moveY → 松手 */
const pullGesture = (target: Element, startY: number, moveY: number) => {
  touch('touchstart', target, startY);
  touch('touchmove', target, moveY);
  touch('touchend', target, moveY);
};

describe('PullRefresh 下拉刷新（M8）', () => {
  it('列表顶部下拉过阈值：指示条跟手增长，松手触发刷新并显示「正在刷新」', async () => {
    let releaseRefresh!: () => void;
    const onRefresh = vi.fn(() => new Promise<void>((resolve) => { releaseRefresh = resolve; }));
    const { el, unmount } = mountPullRefresh(onRefresh);
    const item = el.querySelector('.inner-item')!;

    // 下拉 dy=200，阻尼 0.5 → 指示条 100px（≥ 阈值 64）
    touch('touchstart', item, 100);
    touch('touchmove', item, 300);
    await nextTick();
    const indicator = el.querySelector<HTMLElement>('.pull-refresh-indicator')!;
    expect(indicator.style.height).toBe('100px');
    expect(indicator.textContent).toContain('释放立即刷新');

    touch('touchend', item, 300);
    await nextTick();
    expect(onRefresh).toHaveBeenCalledTimes(1);
    // 刷新中：指示条停留并显示加载指示
    expect(indicator.textContent).toContain('正在刷新');
    expect(indicator.style.height).toBe('40px');

    // 刷新完成：指示条回弹收起
    releaseRefresh();
    await nextTick();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await nextTick();
    expect(indicator.style.height).toBe('0px');
    unmount();
  });

  it('下拉未过阈值：松手回弹，不触发刷新', async () => {
    const onRefresh = vi.fn();
    const { el, unmount } = mountPullRefresh(onRefresh);
    const item = el.querySelector('.inner-item')!;

    // 下拉 dy=60，阻尼 0.5 → 30px < 阈值 64
    pullGesture(item, 100, 160);
    await nextTick();
    expect(onRefresh).not.toHaveBeenCalled();
    expect(el.querySelector<HTMLElement>('.pull-refresh-indicator')!.style.height).toBe('0px');
    unmount();
  });

  it('内部列表未滚到顶部（祖先 scrollTop > 0）：不接管手势，不触发刷新', async () => {
    const onRefresh = vi.fn();
    const { el, unmount } = mountPullRefresh(onRefresh);
    const scroller = el.querySelector<HTMLElement>('.inner-scroll')!;
    const item = el.querySelector('.inner-item')!;
    scroller.scrollTop = 50;

    pullGesture(item, 100, 300);
    await nextTick();
    expect(onRefresh).not.toHaveBeenCalled();
    expect(el.querySelector<HTMLElement>('.pull-refresh-indicator')!.style.height).toBe('0px');
    unmount();
  });

  it('上滑手势：不接管（列表原生滚动），不触发刷新', async () => {
    const onRefresh = vi.fn();
    const { el, unmount } = mountPullRefresh(onRefresh);
    const item = el.querySelector('.inner-item')!;

    pullGesture(item, 300, 100);
    await nextTick();
    expect(onRefresh).not.toHaveBeenCalled();
    unmount();
  });
});
