/**
 * 插件熔断状态机测试（设计文档「熔断与治理」）：
 * - 心跳：ready 后每 5s ping，连续 3 次超时标记「无响应」，恢复后解除；
 * - 崩溃环：60s 窗口内 ready 前错误 ≥3 / 无响应累计 ≥3 → 自动停用；
 * - 停用状态 localStorage 持久化 + 手动重新启用清零计数。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  clearWatchdogCounters,
  createPluginWatchdog,
  getWatchdogCounters
} from '../../plugin-watchdog';
import {
  disablePluginInstance,
  enablePluginInstance,
  getDisabledPluginInstance,
  isPluginInstanceDisabled,
  pluginInstanceKey
} from '../../plugin-disabled';

const KEY = 'weibo-core@org:org_1';

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  clearWatchdogCounters(KEY);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('心跳 watchdog', () => {
  it('连续 3 次 ping 超时标记无响应，恢复后自动解除', async () => {
    let failing = true;
    const ping = vi.fn(() => (failing ? Promise.reject(new Error('ping timeout')) : Promise.resolve()));
    const onUnresponsiveChange = vi.fn();
    const watchdog = createPluginWatchdog({ instanceKey: KEY, ping, onUnresponsiveChange });

    watchdog.startHeartbeat();
    await vi.advanceTimersByTimeAsync(5_000); // 第 1 次超时
    await vi.advanceTimersByTimeAsync(5_000); // 第 2 次超时
    expect(onUnresponsiveChange).not.toHaveBeenCalled();
    expect(watchdog.isUnresponsive()).toBe(false);

    await vi.advanceTimersByTimeAsync(5_000); // 第 3 次超时 → 无响应
    expect(onUnresponsiveChange).toHaveBeenCalledWith(true);
    expect(watchdog.isUnresponsive()).toBe(true);
    expect(getWatchdogCounters(KEY).unresponsiveTrips).toBe(1);

    failing = false;
    await vi.advanceTimersByTimeAsync(5_000); // 恢复
    expect(onUnresponsiveChange).toHaveBeenCalledWith(false);
    expect(watchdog.isUnresponsive()).toBe(false);
    watchdog.dispose();
  });

  it('无响应累计 3 次触发自动停用（崩溃环）', async () => {
    let failing = true;
    const ping = () => (failing ? Promise.reject(new Error('ping timeout')) : Promise.resolve());
    const onAutoDisable = vi.fn();
    const watchdog = createPluginWatchdog({
      instanceKey: KEY,
      ping,
      maxConsecutivePingFailures: 1, // 缩短判定：一次超时即进入无响应
      onAutoDisable
    });

    watchdog.startHeartbeat();
    for (let trip = 0; trip < 2; trip += 1) {
      await vi.advanceTimersByTimeAsync(5_000); // 进入无响应（第 trip+1 次）
      failing = false;
      await vi.advanceTimersByTimeAsync(5_000); // 恢复
      failing = true;
      expect(onAutoDisable).not.toHaveBeenCalled();
    }
    await vi.advanceTimersByTimeAsync(5_000); // 第 3 次无响应 → 自动停用
    expect(onAutoDisable).toHaveBeenCalledWith('unresponsive');
    expect(getWatchdogCounters(KEY).unresponsiveTrips).toBe(3);
    watchdog.dispose();
  });

  it('60s 窗口内 ready 前错误 ≥3 触发自动停用；窗口外不计', () => {
    const onAutoDisable = vi.fn();
    const watchdog = createPluginWatchdog({ instanceKey: KEY, ping: () => Promise.resolve(), onAutoDisable });

    watchdog.recordReadyError();
    vi.advanceTimersByTime(61_000); // 第一次错误滑出窗口
    watchdog.recordReadyError();
    watchdog.recordReadyError();
    expect(onAutoDisable).not.toHaveBeenCalled(); // 窗口内仅 2 次

    watchdog.recordReadyError(); // 窗口内第 3 次 → 自动停用
    expect(onAutoDisable).toHaveBeenCalledWith('ready-errors');
    watchdog.dispose();
  });

  it('计数按实例键累计：dispose 重建不清零（崩溃环语义）', () => {
    const first = createPluginWatchdog({ instanceKey: KEY, ping: () => Promise.resolve() });
    first.recordReadyError();
    first.recordReadyError();
    first.dispose();

    const onAutoDisable = vi.fn();
    const second = createPluginWatchdog({ instanceKey: KEY, ping: () => Promise.resolve(), onAutoDisable });
    second.recordReadyError(); // 累计第 3 次 → 自动停用
    expect(onAutoDisable).toHaveBeenCalledWith('ready-errors');
    second.dispose();
  });
});

describe('停用状态持久化', () => {
  it('disable/enable 与 localStorage 往返；手动重新启用清零计数', () => {
    expect(isPluginInstanceDisabled(KEY)).toBe(false);

    disablePluginInstance(KEY, 'unresponsive');
    expect(isPluginInstanceDisabled(KEY)).toBe(true);
    expect(getDisabledPluginInstance(KEY)?.reason).toBe('unresponsive');
    // 持久化可重读（模拟重启后新一轮读取）
    expect(isPluginInstanceDisabled(KEY)).toBe(true);

    getWatchdogCounters(KEY).unresponsiveTrips = 3;
    enablePluginInstance(KEY);
    expect(isPluginInstanceDisabled(KEY)).toBe(false);
    expect(getWatchdogCounters(KEY).unresponsiveTrips).toBe(0);
  });

  it('实例键按 插件+space 隔离', () => {
    expect(pluginInstanceKey('weibo-core', { type: 'org', id: 'org_1' })).toBe('weibo-core@org:org_1');
    expect(pluginInstanceKey('weibo-core', { type: 'personal', id: 'personal' })).toBe(
      'weibo-core@personal:personal'
    );
    disablePluginInstance('weibo-core@org:org_1', 'ready-errors');
    expect(isPluginInstanceDisabled('weibo-core@org:org_2')).toBe(false);
    expect(isPluginInstanceDisabled('other@org:org_1')).toBe(false);
  });
});
