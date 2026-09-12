/**
 * 事务收藏 store（stores/affairs/affair-favorites，M17 长按次级动作）：
 * 切换往返 + localStorage 落盘；内核 affairs 无收藏接口，仅本机持久化。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { isAffairFavorited, toggleAffairFavorite } from '../../stores/affairs/affair-favorites';

beforeEach(() => {
  localStorage.clear();
});

describe('affair-favorites 事务收藏（M17）', () => {
  it('切换往返 + 落盘', () => {
    expect(isAffairFavorited('affair-x')).toBe(false);
    expect(toggleAffairFavorite('affair-x')).toBe(true);
    expect(isAffairFavorited('affair-x')).toBe(true);

    const persisted = JSON.parse(localStorage.getItem('spark:affair-favorites') ?? '{}');
    expect(persisted['affair-x']).toBe(true);

    expect(toggleAffairFavorite('affair-x')).toBe(false);
    expect(isAffairFavorited('affair-x')).toBe(false);
  });

  it('按 affairId 隔离', () => {
    toggleAffairFavorite('affair-a');
    expect(isAffairFavorited('affair-b')).toBe(false);
  });
});
