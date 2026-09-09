/**
 * 窗口形态断点断言（PC 插件窗口化，ui-layout）：
 * iframe 视口宽 = 窗口内容宽。spark-minichat 断点 560——
 * 侧栏固定 240px + 会话区最小可用 ~320px，两栏并存需 ≥560px；
 * 320 最小窗 / 480 窄窗 / 560 边界均折叠为抽屉，561 起保持两栏。
 */
import { describe, expect, it } from 'vitest';
import { isNarrowLayout } from '../src/ui-layout';

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

describe('spark-minichat ui-layout 窗口形态断点', () => {
  it('窄窗三档（320 最小窗 / 480 窄窗 / 560 边界）侧栏折叠为抽屉', () => {
    for (const width of [320, 480, 560]) {
      setViewport(width);
      expect(isNarrowLayout.value, `width=${width}`).toBe(true);
    }
  });

  it('宽窗（561 起，含 880）保持两栏常驻侧栏', () => {
    for (const width of [561, 880, 1440]) {
      setViewport(width);
      expect(isNarrowLayout.value, `width=${width}`).toBe(false);
    }
  });
});
