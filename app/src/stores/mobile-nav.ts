/**
 * 移动端导航栈（移动端适配波次 2）：窄屏（≤768px）下把桌面多栏布局改为「整页 + 导航栈」——
 * 列表页点开详情 push 一帧变成单独整页，返回栏 pop 回上一栏。
 * 按 tab 各自维护一条页面栈（含顶栏「⋯」里的 settings 等非底部 tab 页，键即 App.vue activeTab），
 * 切 tab 时各栈独立保持：页面组件随 activeTab 的 v-if 销毁后，重进可按栈帧恢复所在层级。
 * 桌面端（isMobileLayout=false）不读写本 store，渲染逻辑与波次 1 前完全一致。
 */
import { reactive, readonly, ref } from 'vue';

/** 栈帧：page 由使用该栈的页面自定义（如 'chat' / 'detail'），params 为简单可序列化数据（会话 id 等） */
export interface MobileNavFrame {
  page: string;
  params?: Record<string, string>;
}

/** 栈动作类型（波次 3 转场方向依据）：resetStack / removeFrames 均归类为 'reset' */
export type MobileNavActionType = 'push' | 'pop' | 'reset';

/** 最近一次栈动作：类型 + 触发 tab */
export interface MobileNavAction {
  type: MobileNavActionType;
  tab: string;
}

const lastAction = ref<MobileNavAction | null>(null);

/**
 * 最近一次栈动作（只读）：供转场组件按动作类型选择动画方向（push=右滑入，pop/reset=反向滑出）。
 * 动作消费后不清空——栈帧 :key 变化时按本记录选方向；空操作（同帧去重 push、栈深 1 的 pop、
 * 无匹配的 removeFrames）不记录，避免选错方向。
 */
export const lastNavAction = readonly(lastAction);

/** 栈底帧：列表页（栈深 1，不显示返回栏） */
const ROOT_FRAME: MobileNavFrame = { page: 'root' };

/** 各 tab 的页面栈，栈底恒为 root 帧 */
const stacks = reactive<Record<string, MobileNavFrame[]>>({});

function stackOf(tab: string): MobileNavFrame[] {
  if (!stacks[tab]) {
    stacks[tab] = [{ ...ROOT_FRAME }];
  }
  return stacks[tab];
}

/** 当前栈顶帧（栈深 1 时为 root 帧） */
export function currentPage(tab: string): MobileNavFrame {
  const stack = stackOf(tab);
  return stack[stack.length - 1];
}

/** 是否可返回上一栏（栈深 >1；返回栏据此决定是否显示） */
export function canBack(tab: string): boolean {
  return stackOf(tab).length > 1;
}

/** 打开详情：压入新栈帧；与栈顶同页同参时不重复压栈（连点同一行不叠栈） */
export function pushPage(tab: string, page: string, params?: Record<string, string>): void {
  const stack = stackOf(tab);
  const top = stack[stack.length - 1];
  const sameParams =
    JSON.stringify(top.params ?? {}) === JSON.stringify(params ?? {});
  if (top.page === page && sameParams) {
    return;
  }
  stack.push(params ? { page, params: { ...params } } : { page });
  lastAction.value = { type: 'push', tab };
}

/** 返回上一栏：弹出栈顶（栈深 1 时不动） */
export function popPage(tab: string): void {
  const stack = stackOf(tab);
  if (stack.length > 1) {
    stack.pop();
    lastAction.value = { type: 'pop', tab };
  }
}

/** 回到栈底列表页（重按当前 tab 等场景） */
export function resetStack(tab: string): void {
  stacks[tab] = [{ ...ROOT_FRAME }];
  lastAction.value = { type: 'reset', tab };
}

/** 移除所有匹配栈帧（栈底 root 帧恒保留）：删除会话/联系人等实体时清掉其所有栈帧（含栈中间） */
export function removeFrames(tab: string, predicate: (frame: MobileNavFrame) => boolean): void {
  const stack = stackOf(tab);
  const kept = stack.filter((frame, index) => index === 0 || !predicate(frame));
  if (kept.length !== stack.length) {
    stacks[tab] = kept;
    lastAction.value = { type: 'reset', tab };
  }
}
