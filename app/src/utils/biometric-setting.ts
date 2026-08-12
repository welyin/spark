const STORAGE_KEY = 'spark.settings.biometricUnlock';

export function isBiometricUnlockEnabled(): boolean {
  if (typeof window === 'undefined' || !window.localStorage) {
    return false;
  }
  return window.localStorage.getItem(STORAGE_KEY) === 'true';
}

export function setBiometricUnlockEnabled(enabled: boolean): void {
  if (typeof window === 'undefined' || !window.localStorage) {
    return;
  }
  if (enabled) {
    window.localStorage.setItem(STORAGE_KEY, 'true');
  } else {
    window.localStorage.removeItem(STORAGE_KEY);
  }
}
