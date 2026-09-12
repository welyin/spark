/**
 * MobileTopBar（M1/M2/M5 走查修正）回归测试：
 * - M1：左上角不再有抽屉按钮（无 open-drawer 事件、无「切换空间」按钮）；
 * - M5：网络状态点位于顶栏左侧区域；
 * - M2：右上 ＋ 下拉卡片含「创建组织」「加入组织」（保留「添加朋友」），点击发出对应事件。
 * 下拉卡片 Teleport 到 body，逐个用例后清理，避免污染下一用例。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import MobileTopBar from '../../components/MobileTopBar.vue';

const mountTopBar = () => {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const emitted: Record<string, number> = {};
  const app = createApp(MobileTopBar, {
    title: '消息',
    onOpenDrawer: () => { emitted['open-drawer'] = (emitted['open-drawer'] ?? 0) + 1; },
    onAddFriend: () => { emitted['add-friend'] = (emitted['add-friend'] ?? 0) + 1; },
    onCreateOrg: () => { emitted['create-org'] = (emitted['create-org'] ?? 0) + 1; },
    onJoinOrg: () => { emitted['join-org'] = (emitted['join-org'] ?? 0) + 1; }
  });
  app.config.warnHandler = () => {};
  app.use(ElementPlus);
  app.mount(el);
  return { el, emitted, unmount: () => { app.unmount(); el.remove(); } };
};

const click = async (target: HTMLElement | null, message: string) => {
  expect(target, message).toBeTruthy();
  target!.dispatchEvent(new Event('click', { bubbles: true }));
  await nextTick();
};

afterEach(async () => {
  document.body.querySelectorAll('.mobile-add-sheet-root, .mobile-search-layer').forEach((n) => n.remove());
  await nextTick();
});

describe('MobileTopBar（M1/M2/M5）', () => {
  it('M1：无左上角抽屉按钮（「切换空间」入口已取消）', () => {
    const { el, unmount } = mountTopBar();
    expect(el.querySelector('button[title="切换空间"]')).toBeNull();
    unmount();
  });

  it('M5：网络状态点在顶栏左侧区域（页名居中、搜索/＋在右）', () => {
    const { el, unmount } = mountTopBar();
    const status = el.querySelector('.mobile-top-bar-status .net-status-dot-btn');
    expect(status).toBeTruthy();
    // 网格三栏顺序：左=状态区，中=页名，右=操作组
    const bar = el.querySelector('.mobile-top-bar')!;
    expect(bar.children[0].classList.contains('mobile-top-bar-status')).toBe(true);
    expect(bar.children[1].classList.contains('mobile-top-bar-title')).toBe(true);
    expect(bar.children[2].classList.contains('mobile-top-bar-actions')).toBe(true);
    unmount();
  });

  it('M2：＋ 下拉卡片含 添加朋友/创建组织/加入组织，点击发出对应事件', async () => {
    const { el, emitted, unmount } = mountTopBar();
    await click(el.querySelector('button[title="添加"]'), '应存在 ＋ 按钮');
    const labels = Array.from(document.querySelectorAll('.mobile-add-sheet-item b')).map((n) => n.textContent);
    // 个人空间（默认）：添加朋友 + 创建/加入组织
    expect(labels).toEqual(['添加朋友', '创建组织', '加入组织']);

    await click(
      Array.from(document.querySelectorAll<HTMLElement>('.mobile-add-sheet-item')).find((n) =>
        n.textContent?.includes('创建组织')
      ) ?? null,
      '应存在「创建组织」菜单项'
    );
    expect(emitted['create-org']).toBe(1);
    // 点击后卡片收起（等 180ms 淡出过渡结束、退场层移除后断言）
    await new Promise((resolve) => setTimeout(resolve, 220));
    await nextTick();
    expect(document.querySelector('.mobile-add-sheet')).toBeNull();

    await click(el.querySelector('button[title="添加"]'), '应存在 ＋ 按钮');
    await click(
      Array.from(document.querySelectorAll<HTMLElement>('.mobile-add-sheet-item')).find((n) =>
        n.textContent?.includes('加入组织')
      ) ?? null,
      '应存在「加入组织」菜单项'
    );
    expect(emitted['join-org']).toBe(1);
    expect(emitted['open-drawer']).toBeUndefined();
    unmount();
  });
});
