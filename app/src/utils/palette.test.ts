// utils/palette 单测：哈希渐变的确定性、空 seed 兜底与输出格式
import { describe, it, expect } from 'vitest';
import { hashGradient } from './palette';

const GRADIENT_RE = /^linear-gradient\(135deg, #[0-9a-f]{6}, #[0-9a-f]{6}\)$/;

describe('hashGradient', () => {
  it('同一 seed 恒得同一配色（确定性）', () => {
    expect(hashGradient('plugin:spark-example')).toBe(hashGradient('plugin:spark-example'));
    expect(hashGradient('root-abc')).toBe(hashGradient('root-abc'));
  });

  it('输出为 135deg 双色渐变格式', () => {
    expect(hashGradient('anything')).toMatch(GRADIENT_RE);
    expect(hashGradient('另一个种子')).toMatch(GRADIENT_RE);
  });

  it('空 seed 兜底为 spark（与调用方约定一致）', () => {
    expect(hashGradient('')).toBe(hashGradient('spark'));
  });

  it('不同 seed 取到不同配色（常见种子不撞车）', () => {
    const results = new Set(['a', 'b', 'c', 'd', 'e', 'f'].map(hashGradient));
    expect(results.size).toBeGreaterThan(1);
  });
});
