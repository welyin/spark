// M4 测试点 3/4：LoginPage 生物识别自动解锁。
// 覆盖：
// - 设置开 + hasSecret → onMounted 自动解锁 → emit('login', password)。
// - user-cancelled → 不弹错，落回密码框并保留手动「使用指纹/人脸」按钮。
// - key-invalidated → 设置项自动关闭 + .el-alert 提示。
// - rootId 标签失配 → 按 no-secret 处理（delete + 关闭设置项 + 提示）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import LoginPage from '../../pages/auth/LoginPage.vue';
import { isMobileLayout } from '../../stores/ui-layout';

function mountLogin(props: Record<string, unknown>, onLogin: (pw: string) => void): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(LoginPage, { ...props, onLogin }) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

function alertText(host: HTMLElement): string {
  return host.querySelector('.el-alert__title')?.textContent ?? '';
}

function bioRetryButton(host: HTMLElement): HTMLElement | null {
  const found = Array.from(host.querySelectorAll<HTMLElement>('.entry-link button')).find((el) =>
    el.textContent?.includes('使用指纹/人脸')
  );
  return found ?? null;
}

beforeEach(() => {
  localStorage.clear();
  isMobileLayout.value = true;
  (window as any).__TAURI_INTERNALS__ = {};
});

afterEach(() => {
  isMobileLayout.value = false;
  delete (window as any).__TAURI_INTERNALS__;
  delete (window as any).electronAPI;
});

describe('LoginPage 生物识别自动解锁', () => {
  it('设置开 + hasSecret：onMounted 自动 unlock → emit login', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    const unlock = vi.fn().mockResolvedValue({ rootId: 'root-m45', password: 'auto-pw-1' });
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true }),
        unlock,
      },
    };
    const onLogin = vi.fn();
    mountLogin({ rootId: 'root-m45', nickname: 'User' }, onLogin);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(unlock).toHaveBeenCalledTimes(1);
    // 第三参 bioSourced=true：指纹来源标记，供 RootGate 跳过口令保鲜（避免两次系统指纹验证）
    expect(onLogin).toHaveBeenCalledWith('auto-pw-1', true);
  });

  it('user-cancelled：不弹错，保留手动按钮且聚焦密码框', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true }),
        unlock: vi.fn().mockRejectedValue('user-cancelled'),
      },
    };
    const onLogin = vi.fn();
    const host = mountLogin({ rootId: 'root-m45' }, onLogin);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onLogin).not.toHaveBeenCalled();
    expect(alertText(host)).toBe(''); // 不弹错误
    expect(bioRetryButton(host)).not.toBeNull(); // 保留手动按钮
    // 设置项未被动过。
    expect(localStorage.getItem('spark.settings.biometricUnlock')).toBe('true');
  });

  it('key-invalidated：设置项自动关闭 + 提示', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    const del = vi.fn().mockResolvedValue({ success: true });
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true }),
        unlock: vi.fn().mockRejectedValue('key-invalidated'),
        delete: del,
      },
    };
    const onLogin = vi.fn();
    const host = mountLogin({ rootId: 'root-m45' }, onLogin);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onLogin).not.toHaveBeenCalled();
    expect(del).toHaveBeenCalled();
    expect(localStorage.getItem('spark.settings.biometricUnlock')).toBeNull(); // 设置项关闭
    expect(alertText(host)).toContain('生物识别已失效');
  });

  it('rootId 标签失配：按 no-secret 处理，关闭设置项 + 提示', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    const del = vi.fn().mockResolvedValue({ success: true });
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true }),
        unlock: vi.fn().mockResolvedValue({ rootId: 'different-root', password: 'pw' }),
        delete: del,
      },
    };
    const onLogin = vi.fn();
    const host = mountLogin({ rootId: 'root-m45' }, onLogin);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onLogin).not.toHaveBeenCalled(); // 标签不匹配不解锁
    expect(del).toHaveBeenCalled(); // 删除错配凭据
    expect(localStorage.getItem('spark.settings.biometricUnlock')).toBeNull();
    expect(alertText(host)).toContain('生物识别已失效');
    expect(bioRetryButton(host)).toBeNull(); // 不再提供手动重试
  });

  it('设置未开：不触发生物识别，直接密码登录', async () => {
    const unlock = vi.fn();
    (window as any).electronAPI = {
      biometric: {
        check: vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true }),
        unlock,
      },
    };
    const onLogin = vi.fn();
    mountLogin({ rootId: 'root-m45' }, onLogin);
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(unlock).not.toHaveBeenCalled();
  });
});
