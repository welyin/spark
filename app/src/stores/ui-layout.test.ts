/**
 * ui-layout 断点与底部 tab 定义（移动端适配波次 1）：
 * tab 顺序/命名是 MobileTabBar 渲染与 App.vue activeTab 映射的契约，改动需同步评审。
 * matchMedia 桩见 test-setup.ts（matches 恒 false → 默认桌面布局）。
 */
import { describe, expect, it } from 'vitest';
import { isMobileLayout, MOBILE_TABS } from './ui-layout';

describe('ui-layout 移动端断点', () => {
  it('底部 tab 固定为 消息/通讯录/应用/我的 四项（与 rail 主导航同源 + MinePage）', () => {
    expect(MOBILE_TABS.map((tab) => tab.id)).toEqual(['messages', 'contacts', 'apps', 'mine']);
    expect(MOBILE_TABS.map((tab) => tab.label)).toEqual(['消息', '通讯录', '应用', '我的']);
  });

  it('jsdom 环境（matchMedia 桩未命中窄屏）默认桌面布局', () => {
    expect(isMobileLayout.value).toBe(false);
  });
});
