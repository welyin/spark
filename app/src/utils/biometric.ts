import { ElMessage } from 'element-plus';
import { isTauri } from '../api';
import { setBiometricUnlockEnabled } from './biometric-setting';
import type { BiometricStatusDto, BiometricUnlockResultDto } from '../api/types';

const SHELL_UNAVAILABLE: BiometricStatusDto = {
  available: false,
  enrolled: false,
  hasSecret: false,
};

function electronApi() {
  return (typeof window !== 'undefined' && window.electronAPI) || undefined;
}

function extractBiometricErrorCode(err: unknown): string | undefined {
  if (typeof err === 'string') {
    return err;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return undefined;
}

const CODE_MESSAGES: Record<string, string> = {
  unsupported: '当前设备或平台不支持生物识别',
  'not-enrolled': '尚未录入指纹/人脸，请在系统设置中录入后重试',
  'no-secret': '未找到已保存的生物识别凭据，请使用密码登录后重新开启',
  'user-cancelled': '已取消生物识别验证',
  'auth-failed': '生物识别验证失败',
  lockout: '生物识别验证已被锁定，请稍后再试',
  'key-invalidated': '生物识别凭据已失效，请使用密码登录后重新开启',
  'invalid-password': '当前密码不正确',
};

export function biometricErrorMessage(err: unknown): string {
  const code = extractBiometricErrorCode(err);
  if (!code) {
    return '生物识别调用失败';
  }
  for (const key of Object.keys(CODE_MESSAGES)) {
    if (code.includes(key)) {
      return CODE_MESSAGES[key];
    }
  }
  return code;
}

export function isBiometricErrorCode(err: unknown, code: string): boolean {
  const msg = extractBiometricErrorCode(err);
  return msg ? msg.includes(code) : false;
}

export async function biometricCheck(): Promise<BiometricStatusDto> {
  const api = electronApi();
  if (!isTauri() || !api?.biometric) {
    return SHELL_UNAVAILABLE;
  }
  try {
    return await api.biometric.check();
  } catch (err) {
    if (isBiometricErrorCode(err, 'unsupported')) {
      return SHELL_UNAVAILABLE;
    }
    throw err;
  }
}

export async function biometricUnlock(): Promise<BiometricUnlockResultDto> {
  const api = electronApi();
  if (!isTauri() || !api?.biometric) {
    throw new Error('unsupported');
  }
  return api.biometric.unlock();
}

export async function biometricStorePassword(password: string): Promise<void> {
  const api = electronApi();
  if (!isTauri() || !api?.biometric) {
    throw new Error('unsupported');
  }
  await api.biometric.storePassword(password);
}

export async function biometricDelete(): Promise<void> {
  const api = electronApi();
  if (!isTauri() || !api?.biometric) {
    return;
  }
  try {
    await api.biometric.delete();
  } catch {
    // 删除失败不阻塞 UI，仅清本地设置项
  }
  setBiometricUnlockEnabled(false);
}

/**
 * 失效兜底：删除已存凭据并关闭设置项，同时给用户一条提示。
 */
export async function biometricInvalidate(reason?: string): Promise<void> {
  await biometricDelete();
  setBiometricUnlockEnabled(false);
  ElMessage.warning(reason ?? '生物识别已失效，请使用密码登录后可重新开启');
}
