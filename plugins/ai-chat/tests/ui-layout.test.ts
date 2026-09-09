/**
 * 窗口形态断点断言（PC 插件窗口化，ui-layout）：
 * iframe 视口宽 = 窗口内容宽。ai-chat 断点 600——
 * 侧栏固定 240px + 聊天区最小可用 ~320px，两栏并存需 ≥560px；
 * 默认窗 480（manifest window.defaultWidth）低于阈值，
 * 故 320 最小窗 / 480 默认窗 / 600 边界均折叠为抽屉，601 起保持两栏。
 */
import { describe, expect, it } from 'vitest';
import { isNarrowLayout } from '../ui-layout';

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

describe('ai-chat ui-layout 窗口形态断点', () => {
  it('窄窗三档（320 最小窗 / 480 默认窗 / 600 边界）侧栏折叠为抽屉', () => {
    for (const width of [320, 480, 600]) {
      setViewport(width);
      expect(isNarrowLayout.value, `width=${width}`).toBe(true);
    }
  });

  it('宽窗（601 起，含 880）保持两栏常驻侧栏', () => {
    for (const width of [601, 880, 1440]) {
      setViewport(width);
      expect(isNarrowLayout.value, `width=${width}`).toBe(false);
    }
  });
});
