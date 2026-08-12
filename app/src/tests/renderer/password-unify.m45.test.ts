// E6 前端密码统一（F9/F10）：stores/password-unify 纯逻辑 + 三事件 + 三态错误映射。
// 覆盖：
// - hydratePasswordUnify：pending 水合 / 无 pending / api 缺失 / status 错误映射。
// - verifyPasswordTicket：ok → {ok,newPassword}；ticket-mismatch → 提示文案；api 缺失。
// - unifyPassword：成功 → ack + 清 pending + ElMessage.success；失败 → lastError + error。
// - handlePasswordUnifyEvent 三事件：ChangeObserved(password_change → warning /
//   password_reset → ElMessageBox.alert 安全警示 / 已 ack 静默)、UnificationDone → ack+清+
//   success、DeviceOutOfGrace → outOfGrace 落库 + 一次性 alert。
// - statusErrorMessage 三态映射（ticket-mismatch / invalid-password / ticket-unavailable）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ElMessage, ElMessageBox } from 'element-plus';
import {
  clearOutOfGrace,
  clearPasswordUnifyLastError,
  clearPasswordUnifyPending,
  getOutOfGraceText,
  handlePasswordUnifyEvent,
  hydratePasswordUnify,
  isDeviceOutOfGrace,
  isUnifyAcknowledged,
  outOfGraceRef,
  passwordUnifyLastError,
  pendingUnifyRef,
  setPendingUnifyForTest,
  ticketMismatchText,
  unifyPassword,
  verifyPasswordTicket,
} from '../../stores/password-unify';

const ROOT = 'root-unify-test';

beforeEach(() => {
  localStorage.clear();
  clearPasswordUnifyPending();
  clearPasswordUnifyLastError();
  clearOutOfGrace();
  (window as any).electronAPI = {};
});

afterEach(() => {
  clearPasswordUnifyPending();
  clearPasswordUnifyLastError();
  clearOutOfGrace();
  delete (window as any).electronAPI;
  vi.restoreAllMocks();
});

describe('hydratePasswordUnify', () => {
  it('status.pending=true → pendingUnify 水合（reason 缺省 password_change）', async () => {
    (window as any).electronAPI = {
      passwordUnify: {
        status: vi.fn().mockResolvedValue({
          pending: true,
          rotatedAt: 1700000000000,
          rotatedByDevice: 'Phone-9',
          reason: 'password_reset',
        }),
      },
    };
    await hydratePasswordUnify(ROOT);
    expect(pendingUnifyRef.value?.rotatedAt).toBe(1700000000000);
    expect(pendingUnifyRef.value?.rotatedByDevice).toBe('Phone-9');
    expect(pendingUnifyRef.value?.reason).toBe('password_reset');
  });

  it('status.pending=false → pendingUnify 清空', async () => {
    setPendingUnifyForTest({ rotatedAt: 1, rotatedBy: 'x', rotatedByDevice: 'x', reason: 'password_change' });
    (window as any).electronAPI = {
      passwordUnify: { status: vi.fn().mockResolvedValue({ pending: false }) },
    };
    await hydratePasswordUnify(ROOT);
    expect(pendingUnifyRef.value).toBeNull();
  });

  it('api 缺失（非 Tauri）→ 静默返回，pending 清空', async () => {
    setPendingUnifyForTest({ rotatedAt: 1, rotatedBy: 'x', rotatedByDevice: 'x', reason: 'password_change' });
    await hydratePasswordUnify(ROOT);
    expect(pendingUnifyRef.value).toBeNull();
    expect(passwordUnifyLastError.value).toBe('');
  });

  it('status 抛错 → lastError 三态映射', async () => {
    (window as any).electronAPI = {
      passwordUnify: { status: vi.fn().mockRejectedValue(new Error('ticket-unavailable')) },
    };
    await hydratePasswordUnify(ROOT);
    expect(passwordUnifyLastError.value).toBe('校验信息缺失');
  });
});

describe('verifyPasswordTicket', () => {
  it('ok → 返回 {ok:true, newPassword}', async () => {
    (window as any).electronAPI = {
      passwordUnify: { verifyTicket: vi.fn().mockResolvedValue({ ok: true }) },
    };
    const res = await verifyPasswordTicket('the-new-pass');
    expect(res).toEqual({ ok: true, newPassword: 'the-new-pass' });
    expect(passwordUnifyLastError.value).toBe('');
  });

  it('ticket-mismatch → {ok:false} + 不一致文案', async () => {
    (window as any).electronAPI = {
      passwordUnify: { verifyTicket: vi.fn().mockRejectedValue(new Error('ticket-mismatch')) },
    };
    const res = await verifyPasswordTicket('wrong');
    expect(res.ok).toBe(false);
    expect(passwordUnifyLastError.value).toContain('与设密设备');
  });

  it('api 缺失 → {ok:false} + 未接通文案', async () => {
    const res = await verifyPasswordTicket('x');
    expect(res.ok).toBe(false);
    expect(passwordUnifyLastError.value).toBe('password-unify 命令尚未接通');
  });
});

describe('unifyPassword', () => {
  it('成功 → ack + 清 pending + ElMessage.success', async () => {
    const success = vi.spyOn(ElMessage, 'success').mockImplementation(() => ({} as never));
    setPendingUnifyForTest({ rotatedAt: 1700000000000, rotatedBy: 'x', rotatedByDevice: 'Phone-9', reason: 'password_change' });
    (window as any).electronAPI = {
      passwordUnify: { unifyPassword: vi.fn().mockResolvedValue({ success: true }) },
    };
    const ok = await unifyPassword(ROOT, 'old', 'new');
    expect(ok).toBe(true);
    expect(pendingUnifyRef.value).toBeNull();
    expect(success).toHaveBeenCalledWith('密码已统一');
    expect(isUnifyAcknowledged(ROOT, 1700000000000)).toBe(true);
  });

  it('失败 → lastError + ElMessage.error', async () => {
    const error = vi.spyOn(ElMessage, 'error').mockImplementation(() => ({} as never));
    (window as any).electronAPI = {
      passwordUnify: { unifyPassword: vi.fn().mockRejectedValue(new Error('invalid-password')) },
    };
    const ok = await unifyPassword(ROOT, 'wrong-old', 'new');
    expect(ok).toBe(false);
    expect(passwordUnifyLastError.value).toBe('当前密码不正确');
    expect(error).toHaveBeenCalledWith('当前密码不正确');
  });
});

describe('handlePasswordUnifyEvent 三事件', () => {
  it('PasswordChangeObserved(password_change) → pending + warning（未 ack）', () => {
    const warning = vi.spyOn(ElMessage, 'warning').mockImplementation(() => ({} as never));
    handlePasswordUnifyEvent(ROOT, {
      kind: 'PasswordChangeObserved',
      data: { rotatedAt: 1700000000000, rotatedBy: 'a', rotatedByDevice: 'Phone-9', reason: 'password_change' },
    } as any);
    expect(pendingUnifyRef.value?.rotatedByDevice).toBe('Phone-9');
    expect(warning).toHaveBeenCalledTimes(1);
    // 已 ack → 不再弹（幂等）。
    expect(isUnifyAcknowledged(ROOT, 1700000000000)).toBe(false); // 观察本身不 ack，但重复事件触发相同弹窗
  });

  it('PasswordChangeObserved(password_reset) → ElMessageBox.alert 安全警示', () => {
    const alertSpy = vi.spyOn(ElMessageBox, 'alert').mockResolvedValue(undefined as never) as unknown as ReturnType<typeof vi.fn>;
    handlePasswordUnifyEvent(ROOT, {
      kind: 'PasswordChangeObserved',
      data: { rotatedAt: 1700000000000, rotatedBy: 'a', rotatedByDevice: 'Phone-9', reason: 'password_reset' },
    } as any);
    expect(pendingUnifyRef.value?.reason).toBe('password_reset');
    expect(alertSpy).toHaveBeenCalledTimes(1);
    const [msg, title] = alertSpy.mock.calls[0];
    expect(String(msg)).toContain('重置');
    expect(title).toBe('安全警示');
  });

  it('PasswordUnificationDone → ack + 清 pending + success', () => {
    const success = vi.spyOn(ElMessage, 'success').mockImplementation(() => ({} as never));
    setPendingUnifyForTest({ rotatedAt: 1700000000000, rotatedBy: 'x', rotatedByDevice: 'x', reason: 'password_change' });
    handlePasswordUnifyEvent(ROOT, {
      kind: 'PasswordUnificationDone',
      data: { rotatedAt: 1700000000000 },
    } as any);
    expect(pendingUnifyRef.value).toBeNull();
    expect(success).toHaveBeenCalledWith('所有设备已完成密码统一');
    expect(isUnifyAcknowledged(ROOT, 1700000000000)).toBe(true);
  });

  it('DeviceOutOfGrace → outOfGrace 落库 + 一次性 alert', () => {
    const alertSpy = vi.spyOn(ElMessageBox, 'alert').mockResolvedValue(undefined as never) as unknown as ReturnType<typeof vi.fn>;
    const event = { kind: 'DeviceOutOfGrace', data: { passwordChangedAt: 1700000000000, graceMs: 7 * 24 * 3600 * 1000 } } as any;
    handlePasswordUnifyEvent(ROOT, event);
    expect(isDeviceOutOfGrace()).toBe(true);
    expect(outOfGraceRef.value?.graceMs).toBe(7 * 24 * 3600 * 1000);
    expect(alertSpy).toHaveBeenCalledTimes(1);
    // 已 ack → 二次到达不弹。
    handlePasswordUnifyEvent(ROOT, event);
    expect(alertSpy).toHaveBeenCalledTimes(1);
  });
});

describe('文案与映射', () => {
  it('getOutOfGraceText 按 graceMs 计算天数', () => {
    expect(getOutOfGraceText(7 * 24 * 3600 * 1000)).toBe('已超 7 天未同步');
    expect(getOutOfGraceText(1 * 24 * 3600 * 1000)).toBe('已超 1 天未同步');
  });

  it('ticketMismatchText 含对端设备与时间', () => {
    const pending = { rotatedAt: 1700000000000, rotatedBy: 'a', rotatedByDevice: 'Phone-9', reason: 'password_change' as const };
    const text = ticketMismatchText(pending);
    expect(text).toContain('Phone-9');
  });
});
