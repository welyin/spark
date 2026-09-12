/**
 * OS 拖入（problem X3/X4/X5）——Tauri file-drop 事件的落点分类与导入预检状态。
 *
 * 链路：tauri.conf 开启 dragDropEnabled → App.vue 监听 tauri://drag-enter/over/drop/leave →
 * 本模块按光标位置 hit-test 分类落点（应用窗口 / 桌面空白 / 左栏空间导航项，落点即域）→
 * DropFeedbackHint 跟随光标显示「目标域名 + 可见范围」（X4）→ 松手开 FileImportPreflight（X5）。
 *
 * 诚实口径：Tauri drag-drop 只给本机路径列表；「文件应用导入收件箱」的内核/文件应用能力
 * 当前不存在（src/api 无文件导入命令、内置插件仅 chat/contacts），故预检确认后只派发
 * `spark:file-import-request` 事件并如实提示「未实际写入」，不伪造导入结果。
 * 文件大小/类型/递归深度需 Rust 侧（fs）读盘后才能核对，预检里标注为待落盘核对。
 */
import { computed, ref } from 'vue';
import { spaceLabelOf, scopeLabelOf, type SpaceRef } from './cross-domain-drop';

/** X3 三类落点 */
export type OsDropZone = 'window' | 'desktop' | 'space-item';

export interface OsDropState {
  active: boolean;
  x: number;
  y: number;
  /** 拖入文件路径（enter/over 事件可能为空数组，drop 一定携带） */
  paths: string[];
  zone: OsDropZone | null;
  /** 落点域（落点即域）；null = 不在任何可承接落点上 */
  target: SpaceRef | null;
  /** 是否可承接（false → 禁止光标） */
  allowed: boolean;
}

/** X5 导入预检会话（FileImportPreflight 数据源） */
export interface ImportPreflightSession {
  paths: string[];
  target: SpaceRef;
  /** 状态机：precheck 预检 → confirmed 已确认（投递事件已派发，实际写入待内核能力） */
  stage: 'precheck' | 'confirmed';
}

/** 导入请求事件（统一收敛点；文件应用/内核承接后消费） */
export const FILE_IMPORT_REQUEST_EVENT = 'spark:file-import-request';

const state = ref<OsDropState>({ active: false, x: 0, y: 0, paths: [], zone: null, target: null, allowed: false });
const preflight = ref<ImportPreflightSession | null>(null);

export const osDropState = computed(() => state.value);
export const importPreflight = computed(() => preflight.value);

/**
 * 落点分类（纯函数，便于测试）：按命中元素向上找
 * 1. [data-drop-space]：左栏空间导航项 → 目标域 = 该项空间（落点即域，X3）
 * 2. [data-drop-zone="desktop"]：桌面空白 → 目标域 = 当前停留空间
 * 3. .window-frame：应用窗口 → 目标域 = 窗口所属空间（桌面窗口均在当前空间）
 * 都不是 → 不可承接。
 */
export function classifyOsDropPoint(hit: Element | null): { zone: OsDropZone; spaceAttr: string | null } | null {
  const spaceItem = hit?.closest?.('[data-drop-space]');
  if (spaceItem) {
    return { zone: 'space-item', spaceAttr: spaceItem.getAttribute('data-drop-space') };
  }
  if (hit?.closest?.('[data-drop-zone="desktop"]')) {
    return { zone: 'desktop', spaceAttr: null };
  }
  if (hit?.closest?.('.window-frame')) {
    return { zone: 'window', spaceAttr: null };
  }
  return null;
}

/** data-drop-space 属性值 → 空间引用（'personal' 或 orgId） */
export function spaceRefFromAttr(attr: string): SpaceRef {
  return attr === 'personal' ? { type: 'personal' } : { type: 'org', orgId: attr };
}

/** drag-enter/over：更新拖拽态（target 由调用方 hit-test + 当前空间回退得出） */
export function updateOsDrop(patch: Partial<Omit<OsDropState, 'active'>>): void {
  state.value = { ...state.value, ...patch, active: true };
}

/** drag-leave / drop 后收尾 */
export function endOsDrop(): void {
  state.value = { ...state.value, active: false, zone: null, target: null, allowed: false, paths: [] };
}

/** 松手：可承接且有文件 → 打开预检；返回是否有落点 */
export function openImportPreflight(): boolean {
  const s = state.value;
  endOsDrop();
  if (!s.allowed || !s.target || s.paths.length === 0) {
    return false;
  }
  preflight.value = { paths: s.paths, target: s.target, stage: 'precheck' };
  return true;
}

/** 预检确认：派发导入请求事件（实际写入待内核/文件应用承接），进入 confirmed 态 */
export function confirmImportPreflight(): void {
  const p = preflight.value;
  if (!p || p.stage !== 'precheck') {
    return;
  }
  window.dispatchEvent(
    new CustomEvent(FILE_IMPORT_REQUEST_EVENT, {
      detail: { paths: p.paths, target: p.target }
    })
  );
  preflight.value = { ...p, stage: 'confirmed' };
}

/** 预检取消 / 关闭 */
export function closeImportPreflight(): void {
  preflight.value = null;
}

export { spaceLabelOf, scopeLabelOf };
