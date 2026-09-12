import { describe, expect, it } from 'vitest';
import { keepTitlebarGrabbable, placedRect, snapAt } from './geometry';

describe('desktop geometry', () => {
  const bounds = { width: 1000, height: 600 };
  it('窗口移动不设边界：屏内位置原样保留，不做夹取', () => {
    expect(keepTitlebarGrabbable({ x: 120, y: 80, w: 400, h: 300 }, bounds)).toEqual({ x: 120, y: 80, w: 400, h: 300 });
  });
  it('唯一兜底：拖出边界后标题栏仍留一小段可抓回（横向 ≥80px，纵向标题栏带 ≥48px）', () => {
    // 向右拖出：只留 80px 在屏内
    expect(keepTitlebarGrabbable({ x: 2000, y: 80, w: 400, h: 300 }, bounds)).toEqual({ x: 920, y: 80, w: 400, h: 300 });
    // 向左拖出：右侧露出 80px
    expect(keepTitlebarGrabbable({ x: -2000, y: 80, w: 400, h: 300 }, bounds)).toEqual({ x: -320, y: 80, w: 400, h: 300 });
    // 向下拖出：标题栏带（44+4）至少留 48px
    expect(keepTitlebarGrabbable({ x: 120, y: 2000, w: 400, h: 300 }, bounds)).toEqual({ x: 120, y: 552, w: 400, h: 300 });
    // 向上拖出：整根标题栏保持在屏内（最多隐去 4px 余量）
    expect(keepTitlebarGrabbable({ x: 120, y: -2000, w: 400, h: 300 }, bounds)).toEqual({ x: 120, y: -4, w: 400, h: 300 });
  });
  it('尺寸归一与拖动无关：保持最小 320×220、不超出工作区', () => {
    expect(keepTitlebarGrabbable({ x: 40, y: 20, w: 100, h: 100 }, bounds)).toEqual({ x: 40, y: 20, w: 320, h: 220 });
    expect(keepTitlebarGrabbable({ x: 40, y: 20, w: 5000, h: 5000 }, bounds)).toEqual({ x: 40, y: 20, w: 1000, h: 600 });
    expect(keepTitlebarGrabbable({ x: 40, y: 20, w: 500, h: 500 }, { width: 280, height: 180 })).toEqual({ x: 40, y: 20, w: 280, h: 180 });
  });
  it('tiles halves and quarters without gaps or overflow', () => {
    expect(placedRect('left', bounds)).toEqual({ x: 0, y: 0, w: 500, h: 600 });
    expect(placedRect('bottom-right', bounds)).toEqual({ x: 500, y: 300, w: 500, h: 300 });
    expect(placedRect('maximized', bounds)).toEqual({ x: 0, y: 0, w: 1000, h: 600 });
  });
  it('chooses corner, side and top snap targets from pointer position', () => {
    expect(snapAt(2, 2, bounds)).toBe('top-left');
    expect(snapAt(998, 598, bounds)).toBe('bottom-right');
    expect(snapAt(998, 300, bounds)).toBe('right');
    expect(snapAt(500, 1, bounds)).toBe('maximized');
    expect(snapAt(500, 300, bounds)).toBe('free');
  });
});
