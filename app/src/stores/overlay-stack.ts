/**
 * 移动端覆盖层栈（Android 前端改造）：详情整页覆盖层（MineDetailContainer / 设置面板内容页等）
 * 不是导航栈帧，但系统回退键必须感知它们——覆盖层打开时按返回键应优先关闭覆盖层，
 * 而不是直接 pop 底层导航栈帧（否则会"跳两层"）。
 *
 * 覆盖层组件在打开/关闭时调用 push/pop 持有 token；App.vue 的 onBackButtonPress 先查本栈：
 * 栈非空 → 派发 spark:close-overlay 事件（detail=栈顶 token），仅栈顶覆盖层响应并自行关闭
 * （叠层时逐层回退，不会一次全关）；栈空 → 走正常导航栈回退。
 */

/** 当前打开的覆盖层数量（0=无覆盖层；仅用于响应式展示/调试） */
import { ref } from 'vue';

export const overlayStackCount = ref(0);

/** 覆盖层 token 栈：栈底→栈顶为打开顺序 */
const tokens: symbol[] = [];

/** 覆盖层打开：登记并返回 token（组件在打开期间持有，关闭/卸载时凭 token 注销） */
export function pushOverlay(): symbol {
  const token = Symbol('mobile-overlay');
  tokens.push(token);
  overlayStackCount.value = tokens.length;
  return token;
}

/** 覆盖层关闭：凭 token 注销（重复注销安全——token 不在栈中视为无操作） */
export function popOverlay(token: symbol | null): void {
  if (!token) {
    return;
  }
  const index = tokens.lastIndexOf(token);
  if (index >= 0) {
    tokens.splice(index, 1);
    overlayStackCount.value = tokens.length;
  }
}

/** 是否有覆盖层打开（系统回退键据此决定先关覆盖层） */
export function hasOverlay(): boolean {
  return tokens.length > 0;
}

/** 请求关闭栈顶覆盖层（App.vue 系统返回键触发；事件 detail 携带栈顶 token，
    各覆盖层组件比对自身 token，仅栈顶响应关闭） */
export function requestCloseOverlay(): void {
  const top = tokens[tokens.length - 1];
  if (top) {
    window.dispatchEvent(new CustomEvent('spark:close-overlay', { detail: top }));
  }
}

/** 判断 close-overlay 事件是否指向本覆盖层（组件事件监听内使用） */
export function isOverlayCloseTarget(event: Event, token: symbol | null): boolean {
  return token !== null && (event as CustomEvent).detail === token;
}
