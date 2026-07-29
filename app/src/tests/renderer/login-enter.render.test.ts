// 登录页提交链路回归测试：回车（keydown.enter.prevent）与点击按钮（native-type=button + @click）
// 都显式调 submit → emit login，完全不经过原生表单提交（webview 隐式提交的默认动作会同步卡主线程，
// 导致「正在登录」蒙版画不出来）；RootGate 收到 login 后先绘制蒙版再 unlock。
import { describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import LoginPage from '../../pages/auth/LoginPage.vue';

function mountLogin(onLogin: (password: string) => void): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(LoginPage, { onLogin }) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function typePassword(host: HTMLElement, value: string): Promise<void> {
  const input = host.querySelector('input')!;
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('登录页提交（回车与点击同路径）', () => {
  it('密码框 keydown.enter 触发 login 事件', async () => {
    const onLogin = vi.fn();
    const host = mountLogin(onLogin);
    await typePassword(host, 'password-123');

    const input = host.querySelector('input')!;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onLogin).toHaveBeenCalledWith('password-123');
  });

  it('点击登录按钮触发 login 事件', async () => {
    const onLogin = vi.fn();
    const host = mountLogin(onLogin);
    await typePassword(host, 'password-123');

    const button = host.querySelector('.submit-btn') as HTMLButtonElement;
    button.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onLogin).toHaveBeenCalledWith('password-123');
  });

  it('原生表单提交被拦截（不跳转、不重复触发）', async () => {
    const onLogin = vi.fn();
    const host = mountLogin(onLogin);
    await typePassword(host, 'password-123');

    const form = host.querySelector('form')!;
    const submitEvent = new Event('submit', { bubbles: true, cancelable: true });
    form.dispatchEvent(submitEvent);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(submitEvent.defaultPrevented).toBe(true);
    expect(onLogin).not.toHaveBeenCalled();
  });
});
