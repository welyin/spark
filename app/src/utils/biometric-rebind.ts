import { ElMessage } from 'element-plus';
import { biometricDelete, biometricStorePassword, isBiometricErrorCode } from './biometric';
import { isBiometricUnlockEnabled, setBiometricUnlockEnabled } from './biometric-setting';
import { runBiometricRitual } from './biometric-ritual';

/**
 * 单一 helper：密码统一成功后删旧 blob 并重新录入生物识别。
 * 若用户取消系统弹窗则静默返回，不计错误。
 */
export async function biometricRebindAfterUnify(rootId: string, password: string): Promise<void> {
  if (!isBiometricUnlockEnabled()) return;

  // 1. 删掉旧 blob
  try {
    await biometricDelete();
  } catch {
    // 旧 blob 可能已失效，删除失败继续重录
  }

  // 2. 用新密码写入新 blob（调一次系统认证）
  try {
    await biometricStorePassword(password);
  } catch (err) {
    if (isBiometricErrorCode(err, 'user-cancelled')) {
      // 用户取消：保留生物识别关闭状态，下次登录可重新开启
      return;
    }
    throw err;
  }

  // 3. 验证一次 rootId 标签，确保新 blob 与当前身份一致
  const ritual = await runBiometricRitual(rootId);
  if (ritual.password === null) {
    // 验证失败或用户取消：关闭设置项，避免下次登录死路
    setBiometricUnlockEnabled(false);
    return;
  }

  setBiometricUnlockEnabled(true);
  ElMessage.success('生物识别凭据已随新密码更新');
}
