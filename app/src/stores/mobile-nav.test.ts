/**
 * mobile-nav 导航栈（移动端适配波次 2）纯逻辑测试：
 * push/pop/reset 行为、canBack/currentPage 口径、各 tab 栈相互独立、同帧去重。
 */
import { describe, expect, it } from 'vitest';
import { canBack, currentPage, popPage, pushPage, removeFrames, resetStack } from './mobile-nav';

describe('mobile-nav 移动端导航栈', () => {
  it('初始为栈深 1 的 root 帧：不可返回，currentPage 即 root', () => {
    expect(currentPage('t-init')).toEqual({ page: 'root' });
    expect(canBack('t-init')).toBe(false);
  });

  it('push 压入详情帧后可返回，pop 回上一栏', () => {
    pushPage('t-msg', 'chat', { id: 'c1' });
    expect(currentPage('t-msg')).toEqual({ page: 'chat', params: { id: 'c1' } });
    expect(canBack('t-msg')).toBe(true);

    pushPage('t-msg', 'profile', { rootId: 'r1' });
    expect(currentPage('t-msg').page).toBe('profile');

    popPage('t-msg');
    expect(currentPage('t-msg')).toEqual({ page: 'chat', params: { id: 'c1' } });
    popPage('t-msg');
    expect(currentPage('t-msg')).toEqual({ page: 'root' });
    expect(canBack('t-msg')).toBe(false);
  });

  it('栈深 1 时 pop 为空操作（不会弹穿栈底）', () => {
    popPage('t-floor');
    expect(currentPage('t-floor')).toEqual({ page: 'root' });
    expect(canBack('t-floor')).toBe(false);
  });

  it('与栈顶同页同参的 push 被去重（连点同一行不叠栈）', () => {
    pushPage('t-dup', 'chat', { id: 'c1' });
    pushPage('t-dup', 'chat', { id: 'c1' });
    popPage('t-dup');
    // 若未去重则此处仍停留在 chat 帧
    expect(currentPage('t-dup')).toEqual({ page: 'root' });
  });

  it('同页不同参的 push 正常叠栈', () => {
    pushPage('t-param', 'chat', { id: 'c1' });
    pushPage('t-param', 'chat', { id: 'c2' });
    expect(currentPage('t-param')).toEqual({ page: 'chat', params: { id: 'c2' } });
    popPage('t-param');
    expect(currentPage('t-param')).toEqual({ page: 'chat', params: { id: 'c1' } });
  });

  it('reset 回到栈底；各 tab 栈相互独立', () => {
    pushPage('t-a', 'chat', { id: 'c1' });
    pushPage('t-b', 'detail', { id: 'app1' });

    resetStack('t-a');
    expect(currentPage('t-a')).toEqual({ page: 'root' });
    // t-b 栈不受 t-a 复位影响（切 tab 各栈独立保持）
    expect(currentPage('t-b')).toEqual({ page: 'detail', params: { id: 'app1' } });
    expect(canBack('t-b')).toBe(true);
  });

  it('removeFrames 清掉所有匹配帧（含栈中间），栈底 root 恒保留', () => {
    pushPage('t-rm', 'chat', { id: 'c1' });
    pushPage('t-rm', 'chat', { id: 'c2' });
    pushPage('t-rm', 'chat', { id: 'c1' });

    // 删除会话 c1：其所有栈帧（栈中间 + 栈顶）一并移除
    removeFrames('t-rm', (frame) => frame.page === 'chat' && frame.params?.id === 'c1');
    expect(currentPage('t-rm')).toEqual({ page: 'chat', params: { id: 'c2' } });
    popPage('t-rm');
    expect(currentPage('t-rm')).toEqual({ page: 'root' });
    expect(canBack('t-rm')).toBe(false);

    // 无匹配帧为空操作；root 帧即使匹配也不移除
    removeFrames('t-rm', () => true);
    expect(currentPage('t-rm')).toEqual({ page: 'root' });
  });
});
