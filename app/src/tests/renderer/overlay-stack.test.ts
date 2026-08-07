/**
 * 覆盖层栈（Android 前端改造）单测：token 制——系统回退键仅关闭栈顶覆盖层，
 * 叠层时逐层回退（修复广播式关闭导致的"跳两层"）。
 */
import { describe, expect, it, vi } from 'vitest';
import {
  hasOverlay,
  isOverlayCloseTarget,
  overlayStackCount,
  popOverlay,
  pushOverlay,
  requestCloseOverlay
} from '../../stores/overlay-stack';

describe('overlay-stack 覆盖层栈', () => {
  it('push/pop 成对维护计数，重复 pop 同一 token 安全', () => {
    const t1 = pushOverlay();
    const t2 = pushOverlay();
    expect(overlayStackCount.value).toBe(2);
    popOverlay(t1);
    expect(overlayStackCount.value).toBe(1);
    // 重复注销同一 token：无操作
    popOverlay(t1);
    expect(overlayStackCount.value).toBe(1);
    // null token：无操作
    popOverlay(null);
    expect(overlayStackCount.value).toBe(1);
    popOverlay(t2);
    expect(overlayStackCount.value).toBe(0);
    expect(hasOverlay()).toBe(false);
  });

  it('requestCloseOverlay 仅派发栈顶 token（叠层逐层关闭）', () => {
    const t1 = pushOverlay();
    const t2 = pushOverlay();
    const handler = vi.fn();
    window.addEventListener('spark:close-overlay', handler);
    try {
      requestCloseOverlay();
      expect(handler).toHaveBeenCalledTimes(1);
      const event = handler.mock.calls[0][0] as CustomEvent;
      expect(isOverlayCloseTarget(event, t2)).toBe(true);
      expect(isOverlayCloseTarget(event, t1)).toBe(false);
      // 模拟栈顶关闭后再请求：下一层成为栈顶
      popOverlay(t2);
      requestCloseOverlay();
      const event2 = handler.mock.calls[1][0] as CustomEvent;
      expect(isOverlayCloseTarget(event2, t1)).toBe(true);
    } finally {
      window.removeEventListener('spark:close-overlay', handler);
      popOverlay(t1);
      popOverlay(t2);
    }
  });

  it('空栈时 requestCloseOverlay 不派发事件', () => {
    const handler = vi.fn();
    window.addEventListener('spark:close-overlay', handler);
    try {
      requestCloseOverlay();
      expect(handler).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener('spark:close-overlay', handler);
    }
  });
});
