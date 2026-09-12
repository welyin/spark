/**
 * ui-layout 断点与底部 tab 定义（移动端适配波次 1）：
 * tab 顺序/命名是 MobileTabBar 渲染与 App.vue activeTab 映射的契约，改动需同步评审。
 * matchMedia 桩见 test-setup.ts（matches 恒 false → 默认桌面布局）。
 */
import { describe, expect, it } from 'vitest';
import { isMobileLayout, MOBILE_TABS } from './ui-layout';

describe('ui-layout 移动端断点', () => {
  it('底部 tab 固定为 消息/事务/空间/应用/设置 五项（M4/M9 走查定稿，docs/ui/problem.md）', () => {
    expect(MOBILE_TABS.map((tab) => tab.id)).toEqual(['messages', 'affairs', 'space', 'apps', 'settings']);
    expect(MOBILE_TABS.map((tab) => tab.label)).toEqual(['消息', '事务', '空间', '应用', '设置']);
  });

  it('jsdom 环境（matchMedia 桩未命中窄屏）默认桌面布局', () => {
    expect(isMobileLayout.value).toBe(false);
  });
});
