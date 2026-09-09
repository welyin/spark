export type Rect = { x: number; y: number; w: number; h: number };
export type Bounds = { width: number; height: number };
export type Placement = 'free' | 'maximized' | 'left' | 'right' | 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right';

export function clampRect(rect: Rect, bounds: Bounds): Rect {
  const width = Math.min(Math.max(rect.w, 320), bounds.width);
  const height = Math.min(Math.max(rect.h, 220), bounds.height);
  return {
    x: Math.max(0, Math.min(rect.x, bounds.width - width)),
    y: Math.max(0, Math.min(rect.y, bounds.height - height)),
    w: width,
    h: height
  };
}

export function placedRect(placement: Exclude<Placement, 'free'>, bounds: Bounds): Rect {
  if (placement === 'maximized') return { x: 0, y: 0, w: bounds.width, h: bounds.height };
  const width = bounds.width / 2;
  const quarter = placement.includes('-');
  const height = quarter ? bounds.height / 2 : bounds.height;
  return { x: placement.endsWith('right') ? width : 0, y: placement.startsWith('bottom') ? height : 0, w: width, h: height };
}

export function snapAt(x: number, y: number, bounds: Bounds): Placement {
  const edge = 20;
  const side = x <= edge ? 'left' : x >= bounds.width - edge ? 'right' : null;
  if (side) {
    if (y <= edge) return `top-${side}`;
    if (y >= bounds.height - edge) return `bottom-${side}`;
    return side;
  }
  return y <= edge ? 'maximized' : 'free';
}