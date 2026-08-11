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
      unlock
    }
  };
});

afterEach(() => {
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

    expect(unlock).toHaveBeenCalledWith('some-password');
    expect(getLastActiveAt()).not.toBeNull();
    // 登录成功进入主界面
    expect(host.querySelector('.app-stub')).toBeTruthy();
    host.remove();
  });
});
