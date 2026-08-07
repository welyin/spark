/**
 * MobileSpaceDrawer 冒烟测试（Android 前端改造）：
 * 侧边栏打开时渲染空间列表/加入创建组织/设置入口；点击遮罩收回；打开状态下可再次切换。
 */
import { afterEach, describe, expect, it } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import MobileSpaceDrawer from '../../components/MobileSpaceDrawer.vue';

const mountDrawer = async (modelValue = true) => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(MobileSpaceDrawer, {
    modelValue,
    'onUpdate:modelValue': () => {}
  });
  app.use(ElementPlus);
  app.mount(el);
  await nextTick();
  return { el, app, unmount: () => { app.unmount(); el.remove(); } };
};

// Teleport 到 body 的内容在 unmount 后异步移除，逐个用例后清理，避免污染下一用例
afterEach(async () => {
  document.body.querySelectorAll('.mobile-drawer-root, .mobile-drawer, .mobile-sheet-root').forEach((n) => n.remove());
  await nextTick();
});

describe('MobileSpaceDrawer 左滑侧边栏', () => {
  it('打开时渲染空间列表、加入/创建组织、设置入口', async () => {
    const { unmount } = await mountDrawer(true);
    // 组件 Teleport 到 body，需在 document 层查询
    const labels = Array.from(document.querySelectorAll('.mobile-drawer-item-label')).map((n) => n.textContent);
    expect(labels).toContain('个人空间');
    expect(labels).toContain('加入/创建组织');
    expect(labels).toContain('设置');
    unmount();
  });

  it('关闭态不渲染抽屉内容', async () => {
    const { unmount } = await mountDrawer(false);
    expect(document.querySelector('.mobile-drawer')).toBeNull();
    unmount();
  });

  it('点击「加入/创建组织」弹出上滑菜单（创建/加入二选一）', async () => {
    const { unmount } = await mountDrawer(true);
    const entry = Array.from(document.querySelectorAll('.mobile-drawer-item')).find((n) =>
      n.textContent?.includes('加入/创建组织')
    );
    expect(entry).toBeTruthy();
    entry!.dispatchEvent(new Event('click', { bubbles: true }));
    await nextTick();
    const sheetItems = Array.from(document.querySelectorAll('.mobile-sheet-item b')).map((n) => n.textContent);
    expect(sheetItems).toEqual(['创建组织', '加入组织']);
    unmount();
  });
});
