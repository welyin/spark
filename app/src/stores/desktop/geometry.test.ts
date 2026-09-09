import { describe, expect, it } from 'vitest';
import { clampRect, placedRect, snapAt } from './geometry';

describe('desktop geometry', () => {
  const bounds = { width: 1000, height: 600 };
  it('clamps all edges and handles a workspace smaller than minimum size', () => {
    expect(clampRect({ x: 950, y: -40, w: 400, h: 300 }, bounds)).toEqual({ x: 600, y: 0, w: 400, h: 300 });
    expect(clampRect({ x: 40, y: 20, w: 500, h: 500 }, { width: 280, height: 180 })).toEqual({ x: 0, y: 0, w: 280, h: 180 });
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