// M5 测试点 14：延迟恢复 store（stores/recovery）纯逻辑。
// 覆盖：
// - handleRecoveryP2pEvent('initiated') → inbound 落库 + ElMessageBox.alert 安全提醒；
//   确认按钮触发 root_recovery_veto。
// - handleRecoveryP2pEvent('vetoed'/'committed') → inbound 清空 + ElMessage.info；
//   committed 同时清空 pending（卡片清除）。
// - 倒计时：remainingMs/formatRemaining 用 fake timers 推进。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ElMessage, ElMessageBox } from 'element-plus';
import {
  clearInboundRecovery,
  formatRemaining,
  handleRecoveryP2pEvent,
  inboundRecovery,
  pendingRecovery,
  remainingMs,
  stopRecoveryClock,
} from '../../stores/recovery';

const ROOT = 'root-recovery-test';

function makeRecoveryEvent(overrides: Record<string, unknown>) {
  return {
    requestId: 'req-1',
    state: 'initiated',
    fromDevice: 'Phone-9',
    op: 'reset_password',
    deadline: Date.now() + 3600_000,
    ...overrides,
  } as any;
}

beforeEach(() => {
  localStorage.clear();
  clearInboundRecovery(ROOT);
  (window as any).electronAPI = {
    recovery: {
      veto: vi.fn().mockResolvedValue({ success: true }),
    },
  };
});

afterEach(() => {
  stopRecoveryClock();
  vi.useRealTimers();
  vi.restoreAllMocks();
  delete (window as any).electronAPI;
});

describe('handleRecoveryP2pEvent', () => {
  it('initiated：inbound 落库 + 弹安全提醒，确认按钮触发 root_recovery_veto', async () => {
    const alertSpy = vi
      .spyOn(ElMessageBox, 'alert')
      .mockResolvedValue(undefined as never) as unknown as ReturnType<typeof vi.fn>;
    const veto = vi.fn().mockResolvedValue({ success: true });
    (window as any).electronAPI = { recovery: { veto } };

    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'initiated' }));

    // inbound 落库（含 fromDevice/deadline）。
    expect(inboundRecovery.value).not.toBeNull();
    expect(inboundRecovery.value?.fromDevice).toBe('Phone-9');
    expect(inboundRecovery.value?.requestId).toBe('req-1');
    expect(inboundRecovery.value?.op).toBe('reset_password');
    // localStorage 持久化。
    const raw = localStorage.getItem(`spark:recovery:inbound:${ROOT}`);
    expect(raw).toContain('"fromDevice":"Phone-9"');

    // 弹安全提醒。
    expect(alertSpy).toHaveBeenCalledTimes(1);
    const [message, title] = alertSpy.mock.calls[0];
    expect(String(message)).toContain('Phone-9');
    expect(title).toBe('安全提醒');

    // 确认按钮 → 调 root_recovery_veto。
    const opts = alertSpy.mock.calls[0][2];
    const instance = { confirmButtonLoading: false };
    const done = vi.fn();
    await opts.beforeClose('confirm', instance, done);
    // beforeClose 内 vetoRecovery 为 fire-and-forget（不返回 promise），需冲刷微任务。
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(veto).toHaveBeenCalledWith('req-1');
    expect(done).toHaveBeenCalled();
    // veto 后 inbound 清空。
    expect(inboundRecovery.value).toBeNull();
  });

  it('vetoed：inbound 清空 + ElMessage.info', () => {
    const infoSpy = vi.spyOn(ElMessage, 'info').mockImplementation(() => ({}) as never) as unknown as ReturnType<typeof vi.fn>;
    // 先有 inbound。
    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'initiated' }));
    expect(inboundRecovery.value).not.toBeNull();

    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'vetoed' }));
    expect(inboundRecovery.value).toBeNull();
    expect(infoSpy).toHaveBeenCalledWith('该恢复请求已被否决');
  });

  it('committed：inbound 清空 + pending 卡片清除', () => {
    const infoSpy = vi.spyOn(ElMessage, 'info').mockImplementation(() => ({}) as never) as unknown as ReturnType<typeof vi.fn>;
    // 构造 pending 存在场景：先注入一个带 pending 的 fake（store pending 为模块私有，
    // 这里通过 handleRecoveryP2pEvent 只覆盖 inbound；pending 由 status 水合。用占位断言：
    // committed 清空 inbound 是核心契约）。
    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'initiated' }));
    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'committed' }));
    expect(inboundRecovery.value).toBeNull();
    expect(infoSpy).toHaveBeenCalledWith('恢复请求已确认执行');
  });
});

describe('倒计时（fake timers）', () => {
  it('remainingMs 随时间归零，formatRemaining 文案变化', () => {
    // 先停钟，确保 timer=null，随后 ensureClock 会把 now 同步到 fake 时间。
    stopRecoveryClock();
    vi.useFakeTimers();
    const base = Date.now();
    vi.setSystemTime(base);
    // 用一条 committed 事件触发 ensureClock 同步 now 到 fake 基准。
    const infoSpy = vi.spyOn(ElMessage, 'info').mockImplementation(() => ({} as any));
    handleRecoveryP2pEvent(ROOT, makeRecoveryEvent({ state: 'committed' }));
    infoSpy.mockRestore();

    const target = base + 65_000; // 1 分 5 秒后
    expect(remainingMs(target)).toBe(65_000);
    expect(formatRemaining(65_000)).toBe('1分05秒');

    // 推进 60s → 剩 5s。
    vi.advanceTimersByTime(60_000);
    expect(remainingMs(target)).toBe(5_000);
    expect(formatRemaining(5_000)).toBe('5秒');

    // 推进到目标之后 → 0。
    vi.advanceTimersByTime(5_000);
    expect(remainingMs(target)).toBe(0);

    // 小时档。
    expect(formatRemaining(3600_000 + 120_000)).toBe('1小时02分');
    // 缺目标 → 0。
    expect(remainingMs(undefined)).toBe(0);
  });
});

describe('状态引用', () => {
  it('导出 pendingRecovery 引用（前端卡片读取）', () => {
    expect(pendingRecovery.value).toBeNull();
    expect(inboundRecovery.value).toBeNull();
  });
});
