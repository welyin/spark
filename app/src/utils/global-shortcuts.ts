/**
 * 桌面端全局快捷键（problem G2 / D12）：
 * - ⌘/Ctrl+1..6 按新左栏顺序切入口：全部消息 / 所有事务 / 空间 / 应用管理 / 系统设置 / 我的；
 * - ⌘/Ctrl+K 全局搜索（DesktopSearch 自挂监听，此处不重复）；
 * - ⌘/Ctrl+N 全局新建：在当前域上下文新建（App.vue 弹「新建」菜单，默认域=当前空间）；
 * - ⌘/Ctrl+F 当前窗口内查找（壳层模态聚焦其内置搜索框；插件窗口壳层不接管）。
 *
 * 纯判定逻辑独立成模块便于单测；副作用（切 tab / 弹层）由 App.vue 承接。
 */

/** ⌘/Ctrl+数字 的入口映射（与 rail 视觉顺序一致：消息·事务·空间·应用·系统设置·我的） */
export const SHORTCUT_TABS = [
  'messages',
  'affairs',
  'space',
  'apps',
  'settings',
  'mine',
] as const;

export type GlobalShortcutAction =
  | { kind: 'switch-tab'; tab: string }
  | { kind: 'new' }
  | { kind: 'find' }
  | { kind: 'none' };

/**
 * 判定一次 keydown 是否为壳层全局快捷键。
 * 仅响应 ⌘/Ctrl（不含 Alt）；⌘K 归 DesktopSearch 组件自管，这里返回 none。
 */
export function resolveGlobalShortcut(event: {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}): GlobalShortcutAction {
  if (!(event.ctrlKey || event.metaKey) || event.altKey) {
    return { kind: 'none' };
  }
  const key = event.key.toLowerCase();
  // D12 全局新建
  if (key === 'n') {
    return { kind: 'new' };
  }
  // G2 窗口内查找
  if (key === 'f') {
    return { kind: 'find' };
  }
  // G2 数字切入口（⌘K 不在这里，见模块注释）
  const index = Number(event.key) - 1;
  if (Number.isInteger(index) && index >= 0 && index < SHORTCUT_TABS.length) {
    return { kind: 'switch-tab', tab: SHORTCUT_TABS[index] };
  }
  return { kind: 'none' };
}
