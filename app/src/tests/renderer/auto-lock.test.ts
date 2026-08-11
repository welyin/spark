// N 天未使用自动锁定（device-trust-and-biometric §5）的纯逻辑单测。
// 存储契约：spark.settings.autoLockDays（0=关闭）/ spark.settings.lastActiveAt（ms）。
import { beforeEach, describe, expect, it } from 'vitest';
import {
  getAutoLockDays,
  setAutoLockDays,
  getLastActiveAt,
  touchLastActiveAt,
  isAutoLockExpired
} from '../../utils/auto-lock';

const DAY = 24 * 60 * 60 * 1000;

describe('auto-lock（N 天未使用自动锁定）', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('默认关闭（0 天）且无活跃记录', () => {
    expect(getAutoLockDays()).toBe(0);
    expect(getLastActiveAt()).toBeNull();
    expect(isAutoLockExpired()).toBe(false);
  });

  it('设置天数后持久化可读回', () => {
    setAutoLockDays(7);
    expect(getAutoLockDays()).toBe(7);
    setAutoLockDays(0);
    expect(getAutoLockDays()).toBe(0);
  });

  it('关闭时永不超时（即使活跃记录久远）', () => {
    setAutoLockDays(0);
    touchLastActiveAt(Date.now() - 100 * DAY);
    expect(isAutoLockExpired()).toBe(false);
  });

  it('开启后未超过 N 天不超时', () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 6 * DAY);
    expect(isAutoLockExpired()).toBe(false);
  });

  it('超过 N 天后超时', () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 8 * DAY);
    expect(isAutoLockExpired()).toBe(true);
  });

  it('边界：恰好满 N 天判定为超时', () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 7 * DAY);
    expect(isAutoLockExpired()).toBe(true);
  });

  it('开启但无活跃记录（首次装新版）不超时', () => {
    setAutoLockDays(7);
    expect(isAutoLockExpired()).toBe(false);
  });

  it('活跃时间刷新后重置计时', () => {
    setAutoLockDays(7);
    touchLastActiveAt(Date.now() - 8 * DAY);
    expect(isAutoLockExpired()).toBe(true);
    touchLastActiveAt();
    expect(isAutoLockExpired()).toBe(false);
  });
});
