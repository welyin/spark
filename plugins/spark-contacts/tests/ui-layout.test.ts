/**
 * 窗口形态断点断言（PC 插件窗口化，ui-architecture §4.2）：
 * iframe 视口宽 = 窗口内容宽。spark-contacts 断点 840——桌面三/四栏
 * （280 + min-width 280 + min-width 280）在 <840px 会横向溢出，
 * 故 769–839px 窗口宽度带同样走移动整页布局（断点高于壳层全局 768，见 src/ui-layout.ts）。
 * 窄窗三档（320 最小窗 / 480 窄窗 / 768 边界）均走移动整页布局。
 */
import { describe, expect, it } from 'vitest';
import { isMobileLayout } from '../src/ui-layout';

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

describe('spark-contacts ui-layout 窗口形态断点', () => {
  it('窄窗三档（320/480/768）均为移动布局（整页 + 插件内导航栈）', () => {
    for (const width of [320, 480, 768]) {
      setViewport(width);
      expect(isMobileLayout.value, `width=${width}`).toBe(true);
    }
  });

  it('839（三栏放不下的最大窗口宽）仍为移动布局', () => {
    setViewport(839);
    expect(isMobileLayout.value).toBe(true);
  });

  it('桌面带（840 起，含默认窗 880）为三/四栏布局', () => {
    for (const width of [840, 880, 1440]) {
      setViewport(width);
      expect(isMobileLayout.value, `width=${width}`).toBe(false);
    }
  });
});
