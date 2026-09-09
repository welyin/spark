/**
 * 窗口形态断点断言（PC 插件窗口化，ui-architecture §4.2）：
 * iframe 视口宽 = 窗口内容宽。spark-chat 断点沿用壳层 768——
 * 桌面两栏（会话列表 280 + 聊天区弹性 min-width:0）在任意 ≥769px 宽度不溢出。
 * 窄窗三档（320 最小窗 / 480 窄窗 / 768 边界）均走移动整页布局。
 */
import { describe, expect, it } from 'vitest';
import { isMobileLayout } from '../src/ui-layout';

function setViewport(width: number): void {
  Object.defineProperty(window, 'innerWidth', { writable: true, configurable: true, value: width });
  window.dispatchEvent(new Event('resize'));
}

describe('spark-chat ui-layout 窗口形态断点', () => {
  it('窄窗三档（320/480/768）均为移动布局（列表/聊天整页切换）', () => {
    for (const width of [320, 480, 768]) {
      setViewport(width);
      expect(isMobileLayout.value, `width=${width}`).toBe(true);
    }
  });

  it('桌面带（769 起，含默认窗 880）为两栏布局', () => {
    for (const width of [769, 880, 1440]) {
      setViewport(width);
      expect(isMobileLayout.value, `width=${width}`).toBe(false);
    }
  });
});
