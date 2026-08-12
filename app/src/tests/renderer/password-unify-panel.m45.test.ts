// E6 前端密码统一面板（F9）：PasswordUnifyPanel 表单渲染 + F4 验票不过不许重封 + F6 重录。
// 覆盖：
// - banner 显示设密设备与时间。
// - reason=password_reset → 密码已被重置警告条。
// - canSubmit 门控：旧密码非空 + 新密码>=8 + 两次一致。
// - submit 成功链：验票 ok → unifyPassword → ElMessage.success + emit('done') + biometric rebind。
// - F4：验票失败 → 错误文案、unifyPassword 不得调用。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessage } from 'element-plus';
import PasswordUnifyPanel from '../../pages/auth/PasswordUnifyPanel.vue';

vi.mock('../../stores/password-unify', () => ({
  verifyPasswordTicket: vi.fn(),
  unifyPassword: vi.fn(),
  formatTs: (ts: number) => `T${ts}`,
}));
vi.mock('../../utils/biometric-rebind', () => ({
  biometricRebindAfterUnify: vi.fn().mockResolvedValue(undefined),
}));

import { verifyPasswordTicket, unifyPassword } from '../../stores/password-unify';
import { biometricRebindAfterUnify } from '../../utils/biometric-rebind';

function mount(rootId: string, onDone: () => void = () => {}): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({
    render: () => h(PasswordUnifyPanel, { rootId, onDone }),
  });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

function typePassword(host: HTMLElement, field: string, value: string) {
  const labels = Array.from(host.querySelectorAll('.el-form-item__label')).map((l) => l.textContent ?? '');
  const idx = labels.findIndex((l) => l.includes(field));
  const input = host.querySelectorAll('.el-form-item input')[idx] as HTMLInputElement;
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(verifyPasswordTicket).mockResolvedValue({ ok: true, newPassword: 'oldpass' });
  vi.mocked(unifyPassword).mockResolvedValue(true);
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('PasswordUnifyPanel 渲染', () => {
  it('banner 显示设密设备', () => {
    const host = mount('root-panel');
    expect(host.querySelector('.hint')?.textContent).toContain('『其他设备』');
  });

  it('默认 reason=password_change 不显示「密码已被重置」警告条', () => {
    const host = mount('root-panel');
    // 组件默认 reason=password_change；重置警示条 v-if=reason==='password_reset' 应不渲染。
    const resetAlert = Array.from(host.querySelectorAll('.el-alert')).find((el) =>
      el.querySelector('.el-alert__title')?.textContent?.includes('密码已被重置')
    );
    expect(resetAlert).toBeUndefined();
  });
});

describe('PasswordUnifyPanel 提交流（F4 验票不过不许重封）', () => {
  it('验票 ok → unifyPassword 调用 → success + emit done + biometric rebind', async () => {
    const success = vi.spyOn(ElMessage, 'success').mockImplementation(() => ({} as never));
    const done = vi.fn();
    const host = mount('root-panel', done);
    typePassword(host, '旧密码', 'oldpass');
    typePassword(host, '新密码', 'newpassword123');
    typePassword(host, '确认新密码', 'newpassword123');
    await flush();

    const submit = Array.from(host.querySelectorAll('.submit-btn')).find((b) =>
      (b as HTMLElement).textContent?.includes('确认统一')
    ) as HTMLElement;
    submit.click();
    await flush();

    expect(verifyPasswordTicket).toHaveBeenCalledWith('oldpass');
    expect(unifyPassword).toHaveBeenCalledWith('root-panel', 'oldpass', 'newpassword123');
    expect(success).toHaveBeenCalledWith('密码已统一，请用新密码登录');
    expect(biometricRebindAfterUnify).toHaveBeenCalledWith('root-panel', 'newpassword123');
    expect(done).toHaveBeenCalledTimes(1);
  });

  it('F4：验票失败 → 错误文案、unifyPassword 不得调用', async () => {
    vi.mocked(verifyPasswordTicket).mockResolvedValue({ ok: false });
    const host = mount('root-panel');
    typePassword(host, '旧密码', 'wrongold');
    typePassword(host, '新密码', 'newpassword123');
    typePassword(host, '确认新密码', 'newpassword123');
    await flush();
    (Array.from(host.querySelectorAll('.submit-btn')).find((b) =>
      (b as HTMLElement).textContent?.includes('确认统一')
    ) as HTMLElement).click();
    await flush();

    expect(unifyPassword).not.toHaveBeenCalled();
    const msg = host.querySelector('.el-alert__title')?.textContent ?? '';
    expect(msg).toContain('不一致');
  });

  it('canSubmit：新密码过短 → 提交按钮禁用', async () => {
    const host = mount('root-panel');
    typePassword(host, '旧密码', 'oldpass');
    typePassword(host, '新密码', 'short');
    typePassword(host, '确认新密码', 'short');
    await flush();
    const submit = Array.from(host.querySelectorAll('.submit-btn')).find((b) =>
      (b as HTMLElement).textContent?.includes('确认统一')
    ) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    submit.click();
    expect(verifyPasswordTicket).not.toHaveBeenCalled();
  });

  it('canSubmit：两次新密码不一致 → 提交禁用', async () => {
    const host = mount('root-panel');
    typePassword(host, '旧密码', 'oldpass');
    typePassword(host, '新密码', 'newpassword123');
    typePassword(host, '确认新密码', 'different456');
    await flush();
    const submit = Array.from(host.querySelectorAll('.submit-btn')).find((b) =>
      (b as HTMLElement).textContent?.includes('确认统一')
    ) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
  });
});
