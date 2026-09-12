export type Rect = { x: number; y: number; w: number; h: number };
export type Bounds = { width: number; height: number };
export type Placement = 'free' | 'maximized' | 'left' | 'right' | 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right';

/** D5 兜底参数：标题栏高度（px，与 --spark-window-titlebar-height 一致） */
export const TITLEBAR_HEIGHT = 44;
/** D5 兜底参数：标题栏带（标题栏 44px + 4px 内容区）在屏内可见高度下限。
    取值 48 即保证整根标题栏始终在屏内（向上最多隐去 4px 余量，向下至少留 48px） */
export const TITLEBAR_GRAB_MIN_VISIBLE = 48;
/** D5 兜底参数：横向至少露出 80px，保证标题栏始终有一小段可抓回 */
export const TITLEBAR_GRAB_MIN_EXPOSED_X = 80;

/** D5 窗口移动不设边界：拖动不再夹取进桌面，唯一兜底 = 标题栏至少留一小段
    在屏内保证能抓回（横向至少露出 TITLEBAR_GRAB_MIN_EXPOSED_X，
    纵向标题栏带至少留 TITLEBAR_GRAB_MIN_VISIBLE）。尺寸仍做合理性归一
    （最小 320×220、不超出工作区），与拖动边界无关。 */
export function keepTitlebarGrabbable(rect: Rect, bounds: Bounds): Rect {
  const width = Math.min(Math.max(rect.w, 320), bounds.width);
  const height = Math.min(Math.max(rect.h, 220), bounds.height);
  return {
    x: Math.max(TITLEBAR_GRAB_MIN_EXPOSED_X - width, Math.min(rect.x, bounds.width - TITLEBAR_GRAB_MIN_EXPOSED_X)),
    y: Math.max(TITLEBAR_HEIGHT - TITLEBAR_GRAB_MIN_VISIBLE, Math.min(rect.y, bounds.height - TITLEBAR_GRAB_MIN_VISIBLE)),
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
