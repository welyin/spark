/**
 * space-visibility 纯逻辑单测（spaces-and-plugins §4）：
 * supportedSpaces 全组合（['org'] / ['personal'] / ['personal','org'] / undefined / 空数组）
 * × 两种空间（personal / org）。
 */
import { describe, expect, it } from 'vitest';
import { isPluginVisibleInSpace } from './space-visibility';

describe('isPluginVisibleInSpace', () => {
  it("['org']：仅组织空间可见", () => {
    expect(isPluginVisibleInSpace(['org'], 'org')).toBe(true);
    expect(isPluginVisibleInSpace(['org'], 'personal')).toBe(false);
  });

  it("['personal']：仅个人空间可见", () => {
    expect(isPluginVisibleInSpace(['personal'], 'personal')).toBe(true);
    expect(isPluginVisibleInSpace(['personal'], 'org')).toBe(false);
  });

  it("['personal','org']：两种空间都可见", () => {
    expect(isPluginVisibleInSpace(['personal', 'org'], 'personal')).toBe(true);
    expect(isPluginVisibleInSpace(['personal', 'org'], 'org')).toBe(true);
  });

  it("未声明（undefined）：按 ['org'] 处理（spaces-and-plugins §4）", () => {
    expect(isPluginVisibleInSpace(undefined, 'org')).toBe(true);
    expect(isPluginVisibleInSpace(undefined, 'personal')).toBe(false);
  });

  it('空数组视同未声明：按 [\'org\'] 处理', () => {
    expect(isPluginVisibleInSpace([], 'org')).toBe(true);
    expect(isPluginVisibleInSpace([], 'personal')).toBe(false);
  });
});
