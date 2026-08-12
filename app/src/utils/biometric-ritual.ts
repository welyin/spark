import { ElMessage } from 'element-plus';
import {
  biometricDelete,
  biometricErrorMessage,
  biometricUnlock,
  isBiometricErrorCode,
} from './biometric';
import { setBiometricUnlockEnabled } from './biometric-setting';

const INVAL_MSG = '生物识别已失效，请用密码登录后可重新开启';

function invalidate() {
  return biometricDelete().finally(() => {
    setBiometricUnlockEnabled(false);
    ElMessage.warning(INVAL_MSG);
  });
}

/**
 * 执行生物识别解锁仪式，返回与目标 rootId 对应的密码。
 * 若 rootId 标签不匹配或凭据失效，自动删除芯片凭据并关闭设置项，避免跨身份冒用。
 * 返回 null 时调用方应让界面落回密码输入；返回的 message 可同步到本地 message ref。
 */
export async function runBiometricRitual(expectedRootId: string): Promise<{ password: string | null; message: string; showRetry: boolean }> {
  try {
    const result = await biometricUnlock();
    if (result.rootId !== expectedRootId) {
      await invalidate();
      return { password: null, message: INVAL_MSG, showRetry: false };
    }
    return { password: result.password, message: '', showRetry: false };
  } catch (err) {
    if (isBiometricErrorCode(err, 'user-cancelled')) {
      return { password: null, message: '', showRetry: true };
    }
    if (isBiometricErrorCode(err, 'key-invalidated') || isBiometricErrorCode(err, 'no-secret')) {
      await invalidate();
      return { password: null, message: INVAL_MSG, showRetry: false };
    }
    const msg = biometricErrorMessage(err);
    ElMessage.error(msg);
    return { password: null, message: msg, showRetry: false };
  }
}
