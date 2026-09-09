/**
 * 窗口管理器核心算法回归（阶段 2 / ui-architecture §4.2）。
 * 验证：开窗幂等、多实例、聚焦置顶、关/最小化后自动激活、单实例恢复。
 * 注：currentSpace 缺省 personal（current-space restoreSpace 兜底），
 * desktopSpaceId 恒 'personal'，全部用例在同一空间桶内断言。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  activeWinKey,
  closeWindow,
  focusWindow,
  minimizeWindow,
  minimizeAllWindows,
  openAppIds,
  openNewWindow,
  openWindow,
  restoreWindows,
  toggleWindow,
  visibleWindows,
  windows
} from './window-manager';

/** 每用例前清空当前空间桶（restoreWindows([]) 清空当前空间实例） */
beforeEach(() => {
  restoreWindows([]);
});

describe('window-manager', () => {
  it('回到桌面保留所有实例，Dock 可恢复任意窗口', () => {
    const first = openWindow('app-a');
    openNewWindow('app-a');
    minimizeAllWindows();
    expect(windows.value).toHaveLength(2);
    expect(visibleWindows.value).toHaveLength(0);
    expect(activeWinKey.value).toBeNull();
    toggleWindow(first);
    expect(activeWinKey.value).toBe(first);
  });
  it('开窗：新建实例并设为前台，zIndex 递增', () => {
    const k1 = openWindow('app-a');
    const k2 = openWindow('app-b');
    expect(windows.value).toHaveLength(2);
    expect(activeWinKey.value).toBe(k2);
    const a = windows.value.find((w) => w.key === k1)!;
    const b = windows.value.find((w) => w.key === k2)!;
    expect(b.zIndex).toBeGreaterThan(a.zIndex);
  });

  it('开窗幂等：同应用已开则复用置顶，不重复开', () => {
    const k1 = openWindow('app-a');
    openWindow('app-b');
    const k1again = openWindow('app-a');
    expect(k1again).toBe(k1);
    expect(windows.value).toHaveLength(2);
    expect(activeWinKey.value).toBe(k1);
  });

  it('多实例：openWindow 幂等复用，openNewWindow 强制多开（同应用两窗）', () => {
    const k1 = openWindow('app-a');
    const k1again = openWindow('app-a');
    expect(k1again).toBe(k1); // 幂等复用
    const k2 = openNewWindow('app-a');
    expect(k2).not.toBe(k1); // 强制新实例
    expect(windows.value.filter((w) => w.appId === 'app-a')).toHaveLength(2);
    expect(activeWinKey.value).toBe(k2);
    // 恢复（决策 1）去重为单实例
    expect(openAppIds()).toEqual(['app-a']);
  });

  it('聚焦置顶：点后台窗置为前台且 z 最大', () => {
    const k1 = openWindow('app-a');
    const k2 = openWindow('app-b');
    focusWindow(k1);
    expect(activeWinKey.value).toBe(k1);
    const a = windows.value.find((w) => w.key === k1)!;
    const b = windows.value.find((w) => w.key === k2)!;
    expect(a.zIndex).toBeGreaterThan(b.zIndex);
  });

  it('关闭前台窗后自动激活剩余非最小化 z 最大者', () => {
    const k1 = openWindow('app-a');
    const k2 = openWindow('app-b');
    minimizeWindow(k1); // a 最小化
    closeWindow(k2); // 关前台 b
    expect(activeWinKey.value).toBeNull(); // a 已最小化，无非最小化窗可激活
    expect(windows.value).toHaveLength(1);
  });

  it('最小化保活：实例保留、minimized=true，不出现在 visibleWindows', () => {
    const k1 = openWindow('app-a');
    openWindow('app-b');
    minimizeWindow(k1);
    expect(windows.value).toHaveLength(2);
    expect(visibleWindows.value.map((w) => w.appId)).toEqual(['app-b']);
    expect(windows.value.find((w) => w.key === k1)!.minimized).toBe(true);
  });

  it('最小化前台窗后激活剩余非最小化 z 最大者', () => {
    const k1 = openWindow('app-a');
    const k2 = openWindow('app-b');
    minimizeWindow(k2); // 最小化前台 b
    expect(activeWinKey.value).toBe(k1);
  });

  it('toggle：前台→最小化，最小化/后台→置顶', () => {
    const k1 = openWindow('app-a');
    toggleWindow(k1); // 前台→最小化
    expect(windows.value.find((w) => w.key === k1)!.minimized).toBe(true);
    toggleWindow(k1); // 最小化→置顶
    const inst = windows.value.find((w) => w.key === k1)!;
    expect(inst.minimized).toBe(false);
    expect(activeWinKey.value).toBe(k1);
  });

  it('单实例恢复（决策 1）：restoreWindows 按 appId 各开一窗', () => {
    openWindow('app-a');
    openWindow('app-b');
    const ids = openAppIds();
    expect(ids.sort()).toEqual(['app-a', 'app-b']);
    restoreWindows(ids);
    expect(windows.value).toHaveLength(2);
    expect(new Set(windows.value.map((w) => w.appId))).toEqual(new Set(['app-a', 'app-b']));
  });
});
