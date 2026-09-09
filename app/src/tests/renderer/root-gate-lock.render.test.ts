// RootGate 自动锁定（device-trust-and-biometric §5）分支回归：
// - 已解锁但超时 → 调 lock() 落回登录页，拒绝进主界面
// - 已解锁未超时 → 进入主界面（App 渲染）
// - 登录成功 → 刷新 lastActiveAt
// 存储契约：spark.settings.autoLockDays（0=关闭）/ spark.settings.lastActiveAt。
// 主界面 App.vue 依赖重（插件宿主/多 store），用 vi.mock 打成占位，聚焦 RootGate 门控逻辑。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// 必须先于 RootGate 的 import 提升 mock 生效：App.vue 打成占位组件
vi.mock('../../App.vue', () => ({
  default: { name: 'AppStub', template: '<div class="app-stub" />' }
}));

import { createApp, h, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import RootGate from '../../RootGate.vue';
import { setAutoLockDays, touchLastActiveAt, getLastActiveAt } from '../../utils/auto-lock';
import { isMobileLayout } from '../../stores/ui-layout';

const DAY = 24 * 60 * 60 * 1000;

type Status = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
  nickname: string | null;
  avatar: string | null;
};

let statusValue: Status;
let lock: ReturnType<typeof vi.fn>;
let unlock: ReturnType<typeof vi.fn>;

async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(RootGate) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

beforeEach(() => {
  localStorage.clear();
  // RootGate.handleLogin → resetGateScroll 调 window.scrollTo / element.scrollTo，jsdom 未实现
  window.scrollTo = (() => {}) as typeof window.scrollTo;
  Element.prototype.scrollTo = (() => {}) as typeof Element.prototype.scrollTo;
  statusValue = { initialized: true, unlocked: true, rootId: 'root-x', nickname: '小明', avatar: null };
  lock = vi.fn().mockResolvedValue(null);
  unlock = vi.fn().mockResolvedValue({ rootId: 'root-x' });
  (window as any).electronAPI = {
    rootIdentity: {
      status: () => Promise.resolve(statusValue),
      lock,
      unlock,
      // 密码考试（A6）：默认非超期（老测试不受影响），单测可覆写
      passwordExamStatus: vi.fn().mockResolvedValue({
        lastPasswordAuth: Date.now(),
        overdue: false,
        intervalMs: 7 * DAY
      })
    },
    biometric: {
      check: vi.fn().mockResolvedValue({ available: false, enrolled: false, hasSecret: false }),
      storePassword: vi.fn().mockResolvedValue(null)
    }
  };
});

afterEach(() => {
  isMobileLayout.value = false;
  delete (window as any).__TAURI_INTERNALS__;
  vi.restoreAllMocks();
});

describe('RootGate 自动锁定门控', () => {
  it('开启且超时：调 lock 落回登录页，不渲染主界面，提示重新输密码', async () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 8 * DAY); // 超时
    const host = mount();
    await flush();

    expect(lock).toHaveBeenCalledTimes(1);
    // 不渲染主界面（App 被 mock 成占位）
    expect(host.querySelector('.app-stub')).toBeNull();
    // 落回登录页（LoginPage 渲染出密码框）
    expect(host.querySelector('input[type="password"]')).toBeTruthy();
    // 提示文案
    expect(host.textContent).toContain('已长时间未使用，请重新输入密码');
    host.remove();
  });

  it('开启未超时：进入主界面，不调 lock', async () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 6 * DAY); // 未超时
    const host = mount();
    await flush();

    expect(lock).not.toHaveBeenCalled();
    expect(host.querySelector('.app-stub')).toBeTruthy();
    host.remove();
  });

  it('自动锁定关闭（默认）：已解锁直接进主界面，不调 lock', async () => {
    // 默认关闭（localStorage 无 autoLockDays = 0）
    touchLastActiveAt(Date.now() - 100 * DAY); // 即便活跃久远也不超时
    const host = mount();
    await flush();

    expect(lock).not.toHaveBeenCalled();
    expect(host.querySelector('.app-stub')).toBeTruthy();
    host.remove();
  });

  it('已锁定态：直接落登录页，不触发 lock', async () => {
    statusValue.unlocked = false;
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 8 * DAY);
    const host = mount();
    await flush();

    expect(lock).not.toHaveBeenCalled(); // 未解锁无需锁
    expect(host.querySelector('.app-stub')).toBeNull();
    expect(host.querySelector('input[type="password"]')).toBeTruthy();
    host.remove();
  });
});

describe('RootGate 登录成功刷新最近活跃时间', () => {
  it('解锁成功后 touchLastActiveAt：lastActiveAt 落盘', async () => {
    statusValue.unlocked = false;
    const host = mount();
    await flush();
    expect(getLastActiveAt()).toBeNull(); // 登录前无记录

    // 触发 handleLogin（密码框回车 → emit login → RootGate handleLogin → unlock → touchLastActiveAt）
    const input = host.querySelector('input[type="password"]') as HTMLInputElement;
    input.value = 'some-password';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await flush();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await flush();

    // bioSourced 第三参：手动输密码 = false（密码考试只认真实密码输入，A6）
    expect(unlock).toHaveBeenCalledWith('some-password', undefined, false);
    expect(getLastActiveAt()).not.toBeNull();
    // 登录成功进入主界面
    expect(host.querySelector('.app-stub')).toBeTruthy();
    host.remove();
  });
});

describe('RootGate 口令保鲜：指纹来源跳过重刷（两次指纹回归）', () => {
  it('手动输密码登录（设置开）：storePassword 保鲜照常执行 1 次', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    (window as any).__TAURI_INTERNALS__ = {}; // isTauri() 为真，biometricStorePassword 走 mock 分支
    statusValue.unlocked = false;
    const host = mount();
    await flush();

    const input = host.querySelector('input[type="password"]') as HTMLInputElement;
    input.value = 'typed-pw';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await flush();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await flush();

    expect(unlock).toHaveBeenCalledWith('typed-pw', undefined, false);
    // 手动密码来源：无 bioSourced 标记，口令保鲜照常（覆盖改密后旧 blob 失配）
    const storePassword = (window as any).electronAPI.biometric.storePassword;
    expect(storePassword).toHaveBeenCalledTimes(1);
    expect(storePassword).toHaveBeenCalledWith('typed-pw');
    host.remove();
  });

  it('指纹解锁登录（设置开 + hasSecret）：只弹一次指纹，跳过 storePassword', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    isMobileLayout.value = true;
    (window as any).__TAURI_INTERNALS__ = {};
    statusValue.unlocked = false;
    (window as any).electronAPI.biometric.check = vi
      .fn()
      .mockResolvedValue({ available: true, enrolled: true, hasSecret: true });
    (window as any).electronAPI.biometric.unlock = vi
      .fn()
      .mockResolvedValue({ rootId: 'root-x', password: 'bio-pw' });
    const storePassword = (window as any).electronAPI.biometric.storePassword;

    const host = mount();
    // LoginPage onMounted → biometricCheck → runBiometricRitual → unlock → emit → handleLogin
    // 是多层异步链，多轮 flush 走完
    await flush();
    await flush();
    await flush();

    // 指纹解锁已弹系统验证并拿到密码 → login('bio-pw', bioSourced=true)
    expect((window as any).electronAPI.biometric.unlock).toHaveBeenCalledTimes(1);
    expect(unlock).toHaveBeenCalledWith('bio-pw', undefined, true);
    // 口令保鲜跳过：不再弹第二次系统指纹验证
    expect(storePassword).not.toHaveBeenCalled();
    // 登录成功进入主界面
    expect(host.querySelector('.app-stub')).toBeTruthy();
    host.remove();
  });

  it('密码考试超期（A6）：挂起自动指纹解锁，提示输一次密码恢复', async () => {
    localStorage.setItem('spark.settings.biometricUnlock', 'true');
    isMobileLayout.value = true;
    statusValue.unlocked = false;
    (window as any).electronAPI.rootIdentity.passwordExamStatus = vi.fn().mockResolvedValue({
      lastPasswordAuth: Date.now() - 8 * 7 * DAY,
      overdue: true,
      intervalMs: 7 * DAY
    });
    const bioCheck = vi.fn().mockResolvedValue({ available: true, enrolled: true, hasSecret: true });
    (window as any).electronAPI.biometric.check = bioCheck;

    const host = mount();
    await flush();
    await flush();

    // 超期 → 不再发起生物识别，提示手动输密码
    expect(bioCheck).not.toHaveBeenCalled();
    expect(host.textContent).toContain('已超过 7 天未输入密码');
    host.remove();
  });
});
