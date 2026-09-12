/**
 * 移动端长按手势（M17/M23，docs/ui/problem.md）：按住 500ms 触发次级动作菜单；
 * 按住期间移动超过 10px（滚动意图）或提前松手/多点触控则取消。
 * 纯触摸实现、不引第三方库；桌面端右键菜单不变，两套触发并存。
 * jsdom 无 TouchEvent 构造器：测试用普通 Event 补 touches 字段模拟（同 pull-refresh.test.ts）。
 */

/** 一组可绑定到模板的长按处理器：@touchstart="lp.start($event, payload)" 等 */
export interface LongPressHandlers<T> {
  start: (event: TouchEvent, payload: T) => void;
  move: (event: TouchEvent) => void;
  end: () => void;
}

const LONG_PRESS_MS = 500;
/** 按住期间允许的手指抖动位移（px），超出视为滚动/拖动，取消长按 */
const MOVE_TOLERANCE = 10;

const touchPoint = (event: TouchEvent): { x: number; y: number } => ({
  x: event.touches[0]?.clientX ?? 0,
  y: event.touches[0]?.clientY ?? 0
});

export function createLongPress<T>(
  onTrigger: (payload: T, event: TouchEvent) => void,
  duration = LONG_PRESS_MS
): LongPressHandlers<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let startX = 0;
  let startY = 0;

  const cancel = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  return {
    start(event, payload) {
      cancel();
      if (event.touches.length !== 1) {
        return;
      }
      const point = touchPoint(event);
      startX = point.x;
      startY = point.y;
      timer = setTimeout(() => {
        timer = null;
        onTrigger(payload, event);
      }, duration);
    },
    move(event) {
      if (timer === null) {
        return;
      }
      const point = touchPoint(event);
      if (Math.abs(point.x - startX) > MOVE_TOLERANCE || Math.abs(point.y - startY) > MOVE_TOLERANCE) {
        cancel();
      }
    },
    end: cancel
  };
}
