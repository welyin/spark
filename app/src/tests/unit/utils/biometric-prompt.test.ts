import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ElMessageBox } from 'element-plus';
import { isMobileLayout } from '../../../stores/ui-layout';
import { promptBiometricBind, setBiometricPromptDismissed, DISMISS_KEY } from '../../../utils/biometric-prompt';

const password = 'login-pw';

function mockStatus(available: boolean, enrolled: boolean, hasSecret: boolean) {
  return {
    biometric: {
      check: vi.fn().mockResolvedValue({ available, enrolled, hasSecret }),
      storePassword: vi.fn().mockResolvedValue({ success: true }),
    },
  };
}

beforeEach(() => {
  localStorage.clear();
  isMobileLayout.value = true;
  (window as any).__TAURI_INTERNALS__ = {};
});

describe('promptBiometricBind', () => {
  it('满足条件：确认后调用 storePassword(登录口令) 并开启设置项', async () => {
    (window as any).electronAPI = mockStatus(true, true, false);
    vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as unknown as import('element-plus').MessageBoxData);

    const result = await promptBiometricBind(password);

    expect(result).toBe(true);
    expect(window.electronAPI.biometric.storePassword).toHaveBeenCalledWith(password);
    expect(localStorage.getItem('spark.settings.biometricUnlock')).toBe('true');
  });

  it('已绑定 hasSecret：不弹窗', async () => {
    (window as any).electronAPI = mockStatus(true, true, true);
    const confirm = vi.spyOn(ElMessageBox, 'confirm');

    const result = await promptBiometricBind(password);

    expect(result).toBe(false);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('生物识别设置项已开启：不弹窗', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    (window as any).electronAPI = mockStatus(true, true, false);
    const confirm = vi.spyOn(ElMessageBox, 'confirm');

    const result = await promptBiometricBind(password);

    expect(result).toBe(false);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('已选不再提示：不弹窗', async () => {
    setBiometricPromptDismissed(true);
    (window as any).electronAPI = mockStatus(true, true, false);
    const confirm = vi.spyOn(ElMessageBox, 'confirm');

    const result = await promptBiometricBind(password);

    expect(result).toBe(false);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('PC 端：不弹窗', async () => {
    isMobileLayout.value = false;
    (window as any).electronAPI = mockStatus(true, true, false);
    const confirm = vi.spyOn(ElMessageBox, 'confirm');

    const result = await promptBiometricBind(password);

    expect(result).toBe(false);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('用户取消系统认证：静默返回 false，设置项不变', async () => {
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: false }),
        storePassword: vi.fn().mockRejectedValue('user-cancelled'),
      },
    };
    vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as unknown as import('element-plus').MessageBoxData);

    const result = await promptBiometricBind(password);

    expect(result).toBe(false);
    expect(localStorage.getItem('spark.settings.biometricUnlock')).toBeNull();
  });
});
