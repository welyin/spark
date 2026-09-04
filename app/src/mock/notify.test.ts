// stores/notify 判流单测（阶段四C android-notifications §2）：前台正在看该
// 会话 / 会话免打扰跳过；同会话 3s 窗口内合并（不逐条弹）。
import { describe, it, expect } from 'vitest';
import { openConversation, closeConversation } from '../stores/messages';
import { shouldNotifyChat } from '../stores/notify';

describe('shouldNotifyChat（前台/免打扰/防抖判流）', () => {
  it('免打扰会话跳过', () => {
    expect(shouldNotifyChat('personal', 'conv-mute', true, 10_000)).toBe(false);
  });

  it('前台正在看该会话跳过；关会话后恢复提醒', () => {
    openConversation('personal', 'conv-active');
    expect(shouldNotifyChat('personal', 'conv-active', false, 20_000)).toBe(false);
    closeConversation('personal');
    expect(shouldNotifyChat('personal', 'conv-active', false, 20_000)).toBe(true);
  });

  it('同会话 3s 内合并（第二次不弹），窗口外恢复', () => {
    const conv = 'conv-debounce';
    expect(shouldNotifyChat('personal', conv, false, 100_000)).toBe(true);
    expect(shouldNotifyChat('personal', conv, false, 101_000)).toBe(false);
    expect(shouldNotifyChat('personal', conv, false, 102_999)).toBe(false);
    expect(shouldNotifyChat('personal', conv, false, 103_000)).toBe(true);
  });
});
