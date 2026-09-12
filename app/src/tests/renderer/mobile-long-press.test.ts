/**
 * 长按手势 helper（components/mobile-long-press，M17/M23）：
 * 按住 500ms 触发；移动超 10px（滚动意图）/ 提前松手 / 多点触控取消。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createLongPress } from '../../components/mobile-long-press';

const touchEvent = (x = 100, y = 200, count = 1): TouchEvent => {
  const event = new Event('touchstart') as TouchEvent;
  Object.defineProperty(event, 'touches', {
    value: Array.from({ length: count }, () => ({ clientX: x, clientY: y }))
  });
  return event;
};

describe('createLongPress 长按手势', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('按住 500ms 触发，携带 payload', () => {
    const onTrigger = vi.fn();
    const lp = createLongPress<string>(onTrigger);
    lp.start(touchEvent(), 'payload-a');
    vi.advanceTimersByTime(499);
    expect(onTrigger).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onTrigger).toHaveBeenCalledTimes(1);
    expect(onTrigger.mock.calls[0][0]).toBe('payload-a');
  });

  it('按住期间移动超 10px（滚动意图）：取消，不触发', () => {
    const onTrigger = vi.fn();
    const lp = createLongPress<string>(onTrigger);
    lp.start(touchEvent(100, 200), 'p');
    lp.move(touchEvent(100, 215));
    vi.advanceTimersByTime(600);
    expect(onTrigger).not.toHaveBeenCalled();
  });

  it('提前松手 / 多点触控：不触发', () => {
    const onTrigger = vi.fn();
    const lp = createLongPress<string>(onTrigger);
    lp.start(touchEvent(), 'p');
    lp.end();
    vi.advanceTimersByTime(600);
    expect(onTrigger).not.toHaveBeenCalled();

    lp.start(touchEvent(100, 200, 2), 'p');
    vi.advanceTimersByTime(600);
    expect(onTrigger).not.toHaveBeenCalled();
  });
});
