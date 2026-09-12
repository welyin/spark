// 桌面端全局快捷键判定（G2/D12）：
// - ⌘/Ctrl+1..6 按新左栏顺序映射：全部消息/所有事务/空间/应用管理/系统设置/我的；
// - ⌘N=全局新建（D12）、⌘F=窗口内查找（G2）；⌘K 不在此模块（DesktopSearch 自管）；
// - 无 ⌘/Ctrl、带 Alt、超界数字一律 none。
import { describe, expect, it } from 'vitest';
import { resolveGlobalShortcut, SHORTCUT_TABS } from '../../utils/global-shortcuts';

const ev = (key: string, patch: Partial<Record<'ctrlKey' | 'metaKey' | 'altKey', boolean>> = {}) => ({
  key,
  ctrlKey: true,
  metaKey: false,
  altKey: false,
  ...patch
});

describe('resolveGlobalShortcut（G2/D12）', () => {
  it('⌘1..⌘6 按新左栏顺序切入口', () => {
    expect(SHORTCUT_TABS).toEqual(['messages', 'affairs', 'space', 'apps', 'settings', 'mine']);
    expect(resolveGlobalShortcut(ev('1'))).toEqual({ kind: 'switch-tab', tab: 'messages' });
    expect(resolveGlobalShortcut(ev('2'))).toEqual({ kind: 'switch-tab', tab: 'affairs' });
    expect(resolveGlobalShortcut(ev('3'))).toEqual({ kind: 'switch-tab', tab: 'space' });
    expect(resolveGlobalShortcut(ev('4'))).toEqual({ kind: 'switch-tab', tab: 'apps' });
    expect(resolveGlobalShortcut(ev('5'))).toEqual({ kind: 'switch-tab', tab: 'settings' });
    expect(resolveGlobalShortcut(ev('6'))).toEqual({ kind: 'switch-tab', tab: 'mine' });
    expect(resolveGlobalShortcut(ev('7'))).toEqual({ kind: 'none' });
    expect(resolveGlobalShortcut(ev('0'))).toEqual({ kind: 'none' });
  });

  it('⌘N 全局新建 / ⌘F 窗口内查找（大小写不敏感）', () => {
    expect(resolveGlobalShortcut(ev('n'))).toEqual({ kind: 'new' });
    expect(resolveGlobalShortcut(ev('N'))).toEqual({ kind: 'new' });
    expect(resolveGlobalShortcut(ev('f'))).toEqual({ kind: 'find' });
    expect(resolveGlobalShortcut(ev('F'))).toEqual({ kind: 'find' });
  });

  it('⌘K 不在此模块（归 DesktopSearch 自管），返回 none', () => {
    expect(resolveGlobalShortcut(ev('k'))).toEqual({ kind: 'none' });
  });

  it('无修饰键 / 带 Alt / 普通字母：none', () => {
    expect(resolveGlobalShortcut(ev('n', { ctrlKey: false }))).toEqual({ kind: 'none' });
    expect(resolveGlobalShortcut(ev('n', { altKey: true }))).toEqual({ kind: 'none' });
    expect(resolveGlobalShortcut(ev('x'))).toEqual({ kind: 'none' });
    expect(resolveGlobalShortcut(ev('1', { metaKey: true, ctrlKey: false }))).toEqual({
      kind: 'switch-tab',
      tab: 'messages'
    });
  });
});
