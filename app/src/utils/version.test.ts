import { describe, expect, it } from 'vitest';
import { compareVersions } from './version';

describe('compareVersions', () => {
  it('比较常规 x.y.z 版本', () => {
    expect(compareVersions('0.2.1', '0.2.2')).toBe(-1);
    expect(compareVersions('0.2.2', '0.2.1')).toBe(1);
    expect(compareVersions('0.2.1', '0.2.1')).toBe(0);
    expect(compareVersions('1.0.0', '0.9.9')).toBe(1);
  });

  it('容忍 v 前缀与预发布/构建段', () => {
    expect(compareVersions('v0.2.2', '0.2.1')).toBe(1);
    expect(compareVersions('0.2.2-beta.1', '0.2.1')).toBe(1);
    expect(compareVersions('0.2.2', '0.2.2+build5')).toBe(0);
  });

  it('缺段按 0 补齐', () => {
    expect(compareVersions('0.2', '0.2.1')).toBe(-1);
    expect(compareVersions('0.2.1', '0.2')).toBe(1);
  });

  it('无法解析时不判定（返回 0）', () => {
    expect(compareVersions('', '0.2.1')).toBe(0);
    expect(compareVersions('abc', '0.2.1')).toBe(0);
  });
});
