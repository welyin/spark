import { ElMessage, ElMessageBox } from 'element-plus';
import { biometricCheck, biometricStorePassword, isBiometricErrorCode } from './biometric';
import { isBiometricUnlockEnabled, setBiometricUnlockEnabled } from './biometric-setting';
import { isMobileLayout } from '../stores/ui-layout';

export const DISMISS_KEY = 'spark.settings.biometricPromptDismissed';

export function isBiometricPromptDismissed(): boolean {
  if (typeof window === 'undefined' || !window.localStorage) return false;
  return window.localStorage.getItem(DISMISS_KEY) === 'true';
}

export function setBiometricPromptDismissed(dismissed: boolean): void {
  if (typeof window === 'undefined' || !window.localStorage) return;
  if (dismissed) {
    window.localStorage.setItem(DISMISS_KEY, 'true');
  } else {
    window.localStorage.removeItem(DISMISS_KEY);
  }
}

/**
 * 移动端登录成功后，若满足条件则引导用户绑定生物识别。
 * @param password 刚验证过的登录口令
 * @returns 是否完成了绑定
 */
export async function promptBiometricBind(password: string): Promise<boolean> {
  if (!isMobileLayout.value || isBiometricPromptDismissed() || isBiometricUnlockEnabled()) {
    return false;
  }

  let status;
  try {
    status = await biometricCheck();
  } catch {
    return false;
  }

  if (!status.available || !status.enrolled || status.hasSecret) {
    return false;
  }

  try {
    const result = await ElMessageBox.confirm(
      '开启指纹/人脸解锁？下次登录可直接使用生物识别，无需输入密码。',
      '生物识别解锁',
      {
        confirmButtonText: '开启',
        cancelButtonText: '暂不',
        type: 'info',
        distinguishCancelAndClose: true,
      }
    );
    if (result !== 'confirm') {
      return false;
    }
  } catch {
    // 关闭弹窗（包括点遮罩、ESC）不算「不再提示」，下次仍提示
    return false;
  }

  try {
    await biometricStorePassword(password);
    setBiometricUnlockEnabled(true);
    ElMessage.success('生物识别解锁已开启');
    return true;
  } catch (err) {
    if (isBiometricErrorCode(err, 'user-cancelled')) {
      // 用户取消系统认证：静默关框，不算不再提示，下次仍提示
      return false;
    }
    throw err;
  }
}