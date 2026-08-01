/**
 * MobileTabBar 点击回归（移动端适配波次 1）：
 * 历史上 setup() 未从上下文解构 emit 导致模板中 emit 为 undefined，
 * 点击静默无效（真机「下导航栏点击不好使」）。本用例直接点击按钮断言事件。
 */
import { describe, expect, it } from 'vitest';
import { createApp } from 'vue';
import MobileTabBar from '../../components/MobileTabBar.vue';

describe('MobileTabBar 点击切换', () => {
  it('点击四个 tab 均向外发出 select 事件并携带正确 id', () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const selected: string[] = [];
    const app = createApp(MobileTabBar, {
      activeTab: 'messages',
      onSelect: (id: string) => selected.push(id)
    });
    app.mount(el);

    const buttons = Array.from(el.querySelectorAll('button'));
    expect(buttons).toHaveLength(4);
    for (const button of buttons) {
      button.dispatchEvent(new Event('click', { bubbles: true }));
    }
    expect(selected).toEqual(['messages', 'contacts', 'apps', 'mine']);

    app.unmount();
    el.remove();
  });
});
