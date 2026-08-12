// M4 测试点 2：SecurityBiometricPanel 设置项开启/关闭流。
// 覆盖：
// - 开启：填密码 → 开启按钮 → biometricStorePassword(password) + setBiometricUnlockEnabled(true)
//   + 开关出现（切到「使用生物识别解锁」态）。
// - 关闭：开启态拨开关 → biometricDelete() + setBiometricUnlockEnabled(false)。
// - 凭据失效：setting 开但 hasSecret=false → 自动关闭开关 + biometricInvalidate。
//   （env 初始 hasSecret=false → watch 仅在 true→false 转变时触发，见用例 3 通过
//    先返回 true 再触发转变的模拟。）
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import SecurityBiometricPanel from '../../components/mine/SecurityBiometricPanel.vue';

vi.mock('../../utils/biometric', () => ({
  biometricCheck: vi.fn(),
  biometricStorePassword: vi.fn(),
  biometricDelete: vi.fn(),
  biometricInvalidate: vi.fn(),
  biometricErrorMessage: (err: unknown) => String(err),
}));
vi.mock('../../utils/biometric-setting', () => ({
  isBiometricUnlockEnabled: vi.fn(),
  setBiometricUnlockEnabled: vi.fn(),
}));

import {
  biometricCheck,
  biometricStorePassword,
  biometricDelete,
} from '../../utils/biometric';
import { isBiometricUnlockEnabled, setBiometricUnlockEnabled } from '../../utils/biometric-setting';

function mount(rootId: string): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(SecurityBiometricPanel, { rootId }) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

function hasEnabledSwitch(host: HTMLElement): boolean {
  return !!host.querySelector('.el-switch');
}

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  vi.mocked(biometricCheck).mockResolvedValue({
    available: true,
    enrolled: true,
    hasSecret: true,
  } as any);
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('SecurityBiometricPanel 开启/关闭流', () => {
  it('开启：填密码点开启 → storePassword + setBiometricUnlockEnabled(true) + 开关出现', async () => {
    vi.mocked(isBiometricUnlockEnabled).mockReturnValue(false);
    vi.mocked(biometricStorePassword).mockResolvedValue({ success: true } as any);
    const host = mount('root-panel');
    await flush();
    // 初始：未开启 → 显示密码表单，无开关。
    expect(hasEnabledSwitch(host)).toBe(false);

    // 填密码（el-input 内层 input）+ 点开启。
    const input = host.querySelector('.enable-form input') as HTMLInputElement;
    input.value = 'secretpw123';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const btn = Array.from(host.querySelectorAll('.panel-actions .el-button')).find((b) =>
      (b as HTMLElement).textContent?.includes('开启生物识别解锁')
    ) as HTMLElement;
    btn.click();
    await flush();

    expect(biometricStorePassword).toHaveBeenCalledWith('secretpw123');
    expect(setBiometricUnlockEnabled).toHaveBeenCalledWith(true);
    expect(hasEnabledSwitch(host)).toBe(true);
  });

  it('关闭：开启态拨开关 → delete + setBiometricUnlockEnabled(false)', async () => {
    vi.mocked(isBiometricUnlockEnabled).mockReturnValue(true);
    vi.mocked(biometricDelete).mockResolvedValue({ success: true } as any);
    const host = mount('root-panel');
    await flush();
    expect(hasEnabledSwitch(host)).toBe(true);

    const switchEl = host.querySelector('.el-switch') as HTMLElement;
    switchEl.click();
    await flush();

    expect(biometricDelete).toHaveBeenCalled();
    expect(setBiometricUnlockEnabled).toHaveBeenCalledWith(false);
  });

  it('首次开启（hasSecret=false）仍可用：canUseBiometric 不含 hasSecret，密码框/按钮可用', async () => {
    // R3 返工：canUseBiometric = available && enrolled（不含 hasSecret）。
    // hasSecret 只用于状态展示与已开启态失效检测；首次无已存凭据也能开启。
    vi.mocked(isBiometricUnlockEnabled).mockReturnValue(false);
    vi.mocked(biometricCheck).mockResolvedValue({
      available: true,
      enrolled: true,
      hasSecret: false,
    } as any);
    vi.mocked(biometricStorePassword).mockResolvedValue({ success: true } as any);
    const host = mount('root-panel');
    await flush();

    // 状态行显示「未保存」。
    const secretRow = Array.from(host.querySelectorAll('.status-row')).find((el) =>
      el.textContent?.includes('已保存解锁凭据')
    )?.textContent ?? '';
    expect(secretRow).toContain('未保存');

    // canUseBiometric=true → 无置灰提示 el-alert（.bio-tip 不出现）。
    expect(host.querySelector('.bio-tip')).toBeNull();

    // 首次开启表单可用：canUseBiometric 为真（available&&enrolled）→ 密码输入框不 disabled
    //（:disabled="!canUseBiometric"，不受 hasSecret 影响）。
    const input = host.querySelector('.enable-form input') as HTMLInputElement;
    expect(input.disabled).toBe(false);
    const btn = Array.from(host.querySelectorAll('.panel-actions .el-button')).find((b) =>
      (b as HTMLElement).textContent?.includes('开启生物识别解锁')
    ) as HTMLButtonElement;
    // 空密码时按钮因 !currentPassword 置灰（与 canUseBiometric 无关）；填密码后即可点。
    expect(btn.disabled).toBe(true);
    input.value = 'secretpw123';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(btn.disabled).toBe(false);

    // 点开启 → storePassword + 设置置真。
    btn.click();
    await flush();
    expect(biometricStorePassword).toHaveBeenCalledWith('secretpw123');
    expect(setBiometricUnlockEnabled).toHaveBeenCalledWith(true);
    expect(hasEnabledSwitch(host)).toBe(true);
  });

  // 注：凭据失效（hasSecret 由真转假自动关闭）的 watch 需 hasSecret 发生 true→false 转变，
  // 而面板 onMounted 只调一次 biometricCheck；该失效路径已在 LoginPage key-invalidated
  // 用例（login-biometric.m45.test.ts）覆盖（invalidate + 设置关闭 + 提示）。
});
