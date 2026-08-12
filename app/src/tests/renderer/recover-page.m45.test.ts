// M5 测试点 15：RecoverPage 移动端 tab 三态 + PC 端无 tab 回归。
// 覆盖：
// - 移动端（isMobileLayout=true）：显示 助记词/延迟恢复 两个 tab；
//   pendingRecovery 三态 → initiated「公示中」/ vetoed「已否决」/ 无 pending「没有进行中」。
// - PC 端（isMobileLayout=false）：不渲染 .recover-tabs，渲染 .recover-deadend 引导。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import RecoverPage from '../../pages/auth/RecoverPage.vue';
import { isMobileLayout } from '../../stores/ui-layout';
import { stopRecoveryClock } from '../../stores/recovery';

function mount(props: Record<string, unknown>): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({
    render: () => h(RecoverPage, { backLabel: '返回', ...props }),
  });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function tabLabels(host: HTMLElement): string[] {
  return Array.from(host.querySelectorAll('.recover-tab')).map((el) => el.textContent ?? '');
}

beforeEach(() => {
  localStorage.clear();
  stopRecoveryClock();
  (window as any).electronAPI = { rootIdentity: {} };
});

afterEach(() => {
  isMobileLayout.value = false;
  stopRecoveryClock();
  document.body.innerHTML = '';
  delete (window as any).electronAPI;
});

describe('RecoverPage 移动端 tab 三态', () => {
  it('移动端渲染两个 tab，默认落在助记词', async () => {
    isMobileLayout.value = true;
    const host = mount({});
    await flush();
    expect(tabLabels(host)).toEqual(['助记词', '延迟恢复']);
    // 默认 activeTab 为助记词。
    const active = host.querySelector('.recover-tab.active')?.textContent;
    expect(active).toBe('助记词');
  });

  it('pendingRecovery=initiated：延迟恢复 tab 显示「公示中」+ 请求ID', async () => {
    isMobileLayout.value = true;
    (window as any).electronAPI = {
      rootIdentity: {},
      recovery: {
        status: vi.fn().mockResolvedValue({
          readyToConfirm: false,
          pending: {
            requestId: 'rc-abc',
            state: 'initiated',
            op: 'reset_password',
            initiatedAt: Date.now() - 1000,
            deadline: Date.now() + 60_000,
          },
        }),
      },
    };
    const host = mount({ rootId: 'root-rec' });
    await flush();
    // 切到延迟恢复 tab。
    const delayTab = Array.from(host.querySelectorAll('.recover-tab')).find((el) =>
      el.textContent?.includes('延迟恢复')
    ) as HTMLElement;
    delayTab.click();
    await flush();

    const text = host.querySelector('.delay-status')?.textContent ?? '';
    expect(text).toContain('rc-abc');
    expect(text).toContain('公示中');
    expect(text).toContain('剩余时间');
  });

  it('pendingRecovery=vetoed：显示「已否决」', async () => {
    isMobileLayout.value = true;
    (window as any).electronAPI = {
      rootIdentity: {},
      recovery: {
        status: vi.fn().mockResolvedValue({
          readyToConfirm: false,
          pending: {
            requestId: 'rc-veto',
            state: 'vetoed',
            op: 'reset_password',
            initiatedAt: Date.now() - 1000,
            deadline: Date.now() + 1000,
          },
        }),
      },
    };
    const host = mount({ rootId: 'root-rec' });
    await flush();
    const delayTab = Array.from(host.querySelectorAll('.recover-tab')).find((el) =>
      el.textContent?.includes('延迟恢复')
    ) as HTMLElement;
    delayTab.click();
    await flush();
    const text = host.querySelector('.delay-status')?.textContent ?? '';
    expect(text).toContain('已否决');
  });

  it('pendingRecovery 为空：显示「没有进行中」引导', async () => {
    isMobileLayout.value = true;
    // 显式 hydration 为空，复位模块级单例 pending。
    (window as any).electronAPI = {
      rootIdentity: {},
      recovery: {
        status: vi.fn().mockResolvedValue({ readyToConfirm: false, pending: null }),
      },
    };
    const host = mount({ rootId: 'root-rec' });
    await flush();
    const delayTab = Array.from(host.querySelectorAll('.recover-tab')).find((el) =>
      el.textContent?.includes('延迟恢复')
    ) as HTMLElement;
    delayTab.click();
    await flush();
    const text = host.querySelector('.delay-status')?.textContent ?? '';
    expect(text).toContain('没有进行中的延迟恢复请求');
  });
});

describe('RecoverPage PC 端无 tab 回归', () => {
  it('PC 端（isMobileLayout=false）：不渲染 tab，渲染 recover-deadend 引导', async () => {
    isMobileLayout.value = false;
    const host = mount({});
    await flush();
    expect(host.querySelector('.recover-tabs')).toBeNull();
    expect(host.querySelector('.recover-deadend')).not.toBeNull();
    const deadend = host.querySelector('.recover-deadend')?.textContent ?? '';
    expect(deadend).toContain('没有助记词');
    // PC 端仍然渲染助记词恢复表单。
    expect(host.querySelector('.auth-steps')).not.toBeNull();
  });
});
