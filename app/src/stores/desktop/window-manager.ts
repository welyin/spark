/**
 * PC 空间桌面 · 窗口管理器（阶段 2 / ui-architecture §4.2 两级状态分离之「窗口实例表」）。
 *
 * 蓝本 ark-desktop-main（shell-desktop §3.5 论证采用），Spark 改动：
 *  - 实例表按空间分桶：每张空间各持一份窗口集，切空间 = 整体换一桶（ark 全局单桌面）；
 *  - 同应用多实例：winKey = `${spaceId}|${appId}|${seq}`（ark 单例 appId）。
 *
 * 状态只记「开着哪些窗、谁在前台、是否最小化、级联序号」；位置/尺寸由各窗口组件
 * 本地持有（参照 ark，rect 不进全局表）。持久化恢复策略（§八决策 1）：只还原
 * 「上次开着哪些应用」、每应用恢复单实例，seq 不还原。
 */
import { computed, reactive, ref, watch } from 'vue';
import { desktopSpaceId } from './app-registry';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';

/** 窗口实例（全局表项；rect 由 WindowFrame 本地持有，不入此表） */
export interface WinInst {
  viewId?: string;
  viewBootstrap?: { cardData?: unknown };
  /** 窗口键（`${spaceId}|${appId}|${seq}`） */
  key: string;
  /** 所属应用 id */
  appId: string;
  /** 多实例序号（同应用第 seq 次打开） */
  seq: number;
  /** 层级（单调递增，越大越靠前） */
  zIndex: number;
  /** 是否最小化（v-show 隐藏保活，不卸载） */
  minimized: boolean;
}

/** 每张空间一份窗口集 */
interface SpaceWindows {
  /** 实例表 Map<winKey, WinInst> */
  instances: Map<string, WinInst>;
  /** 前台窗口 key（null=无前台窗） */
  activeKey: string | null;
  /** 层级计数器（单调递增） */
  maxZ: number;
  /** 实例序号计数器（按 appId 递增，保证 winKey 唯一） */
  seqCounter: Record<string, number>;
  /** 已开窗数（级联偏移依据） */
  openCount: number;
}

function emptySpaceWindows(): SpaceWindows {
  return { instances: new Map(), activeKey: null, maxZ: 0, seqCounter: {}, openCount: 0 };
}

/** 全部空间的窗口集（Map<spaceId, SpaceWindows>），惰性建桶 */
const spaces = reactive(new Map<string, SpaceWindows>());

function bucket(spaceId: string): SpaceWindows {
  let b = spaces.get(spaceId);
  if (!b) {
    b = emptySpaceWindows();
    spaces.set(spaceId, b);
  }
  return spaces.get(spaceId)!;
}

/** 当前空间的窗口集（响应式：随 desktopSpaceId 切换换桶） */
const current = computed<SpaceWindows>(() => bucket(desktopSpaceId.value));

export const windowGroups = computed(() => [...spaces.entries()].map(([spaceId, value]) => ({
  space: { type: spaceId === 'personal' ? 'personal' : 'org', id: spaceId } as PluginSpaceContext,
  windows: [...value.instances.values()]
})));

function ownerOf(key: string): SpaceWindows | undefined {
  return [...spaces.values()].find((value) => value.instances.has(key));
}

/** 当前空间全部窗口（渲染用，按 zIndex 升序） */
export const windows = computed<WinInst[]>(() =>
  [...current.value.instances.values()].sort((a, b) => a.zIndex - b.zIndex)
);

/** 当前前台窗口 key */
export const activeWinKey = computed<string | null>(() => current.value.activeKey);

/** 当前空间非最小化窗口（Dock/任务栏激活态依据） */
export const visibleWindows = computed<WinInst[]>(() =>
  windows.value.filter((w) => !w.minimized)
);

/**
 * 打开应用窗口（幂等，默认单实例语义）。
 * - 该应用已有非最小化窗：置顶激活（不重复开）；
 * - 全是最小化窗：取消最小化第一个并置顶；
 * - 否则：新建实例（seq 递增），级联偏移由组件按 openCount 计算。
 * 返回窗口 key（新建或复用的）。
 */
export function openWindow(appId: string): string {
  const b = current.value;
  const existing = [...b.instances.values()].filter((w) => w.appId === appId);
  const reusable = existing.find((w) => !w.minimized) ?? existing[0];
  if (reusable) {
    reusable.minimized = false;
    focusWindow(reusable.key);
    return reusable.key;
  }
  return openNewWindow(appId);
}

/**
 * 强制开新实例（同应用多开，如同时看两个事务；shell-desktop §3.2「同一应用可开多窗口」）。
 * 不做幂等复用，直接建 seq 递增的新实例。
 */
export function openNewWindow(appId: string, options: Pick<WinInst, 'viewId' | 'viewBootstrap'> = {}): string {
  const b = current.value;
  const seq = (b.seqCounter[appId] ?? 0) + 1;
  b.seqCounter[appId] = seq;
  const key = `${desktopSpaceId.value}|${appId}|${seq}`;
  const inst: WinInst = { key, appId, seq, zIndex: ++b.maxZ, minimized: false, ...options };
  b.instances.set(key, inst);
  b.activeKey = key;
  b.openCount += 1;
  return key;
}

/** 聚焦窗口：置顶（zIndex = ++maxZ）并设为前台 */
export function focusWindow(key: string): void {
  const b = ownerOf(key);
  if (!b) return;
  const inst = b.instances.get(key);
  if (!inst) {
    return;
  }
  inst.minimized = false;
  inst.zIndex = ++b.maxZ;
  b.activeKey = key;
}

/** 关闭窗口：移除实例；若关的是前台窗，激活剩余非最小化窗中 z 最大者（不留空窗期） */
export function closeWindow(key: string): void {
  const b = ownerOf(key);
  if (!b) return;
  const wasActive = b.activeKey === key;
  b.instances.delete(key);
  b.openCount = Math.max(0, b.openCount - 1);
  if (!wasActive) {
    return;
  }
  const next = [...b.instances.values()].filter((w) => !w.minimized).sort((a, c) => c.zIndex - a.zIndex)[0];
  b.activeKey = next ? next.key : null;
}

export function closeAppWindows(appId: string): void {
  for (const group of windowGroups.value) {
    for (const inst of group.windows) {
      if (inst.appId === appId) closeWindow(inst.key);
    }
  }
}

/** 最小化窗口（v-show 隐藏保活）；若最小化的是前台窗，激活剩余非最小化 z 最大者 */
export function minimizeWindow(key: string): void {
  const b = ownerOf(key);
  if (!b) return;
  const inst = b.instances.get(key);
  if (!inst) {
    return;
  }
  inst.minimized = true;
  if (b.activeKey === key) {
    const next = [...b.instances.values()].filter((w) => !w.minimized).sort((a, c) => c.zIndex - a.zIndex)[0];
    b.activeKey = next ? next.key : null;
  }
}

/** 切换窗口显隐（Dock 三态之「前台→最小化 / 后台→置顶」在组件层组合 open/focus/minimize） */
export function toggleWindow(key: string): void {
  const b = ownerOf(key);
  if (!b) return;
  const inst = b.instances.get(key);
  if (!inst) {
    return;
  }
  if (b.activeKey === key && !inst.minimized) {
    minimizeWindow(key);
  } else {
    focusWindow(key);
  }
}

export function minimizeAllWindows(): void {
  for (const inst of current.value.instances.values()) {
    inst.minimized = true;
  }
  current.value.activeKey = null;
}

/** 新窗口级联序号（组件据此算 +N×20px 阶梯偏移；参照 ark 级联，Spark 取 openCount-1） */
export function cascadeIndex(key: string): number {
  const b = ownerOf(key);
  if (!b) return 0;
  const inst = b.instances.get(key);
  return inst ? Math.max(0, b.openCount - 1) : 0;
}

/**
 * 导出当前空间「开着哪些应用」（去重 appId 列表），供持久化恢复（决策 1：每应用单实例）。
 */
export function openAppIds(spaceId?: string): string[] {
  const b = bucket(spaceId ?? desktopSpaceId.value);
  return [...new Set([...b.instances.values()].map((w) => w.appId))];
}

/** 恢复空间窗口（每应用单实例）：清空后按 appId 各开一窗 */
export function restoreWindows(appIds: string[], spaceId?: string): void {
  const sid = spaceId ?? desktopSpaceId.value;
  const b = bucket(sid);
  b.instances.clear();
  b.activeKey = null;
  b.openCount = 0;
  // 在当前空间才建实例；非当前空间仅清桶（恢复由进入该空间时触发）
  if (sid !== desktopSpaceId.value) {
    return;
  }
  for (const appId of appIds) {
    openWindow(appId);
  }
}

/* ---- 窗口状态持久化（2.7 / 决策 1：重启只还原「上次开着哪些应用」、每应用单实例） ----
   存本机 localStorage、按空间隔离、不跨端（shell-desktop §3.5(4)）。
   键 `spark:windows:<spaceId>` = { open: string[] }（去重 appId，不含 seq/位置）。 */

function windowsStorageKey(spaceId: string): string {
  return `spark:windows:${spaceId}`;
}

function loadPersistedOpenApps(spaceId: string): string[] {
  try {
    const raw = localStorage.getItem(windowsStorageKey(spaceId));
    if (raw) {
      const parsed = JSON.parse(raw) as { open?: string[] };
      return Array.isArray(parsed.open) ? parsed.open : [];
    }
  } catch {
    // 损坏数据按空处理
  }
  return [];
}

/** 保存当前空间开着的应用（去重 appId）到本机 */
export function persistWindows(spaceId?: string): void {
  const sid = spaceId ?? desktopSpaceId.value;
  try {
    localStorage.setItem(windowsStorageKey(sid), JSON.stringify({ open: openAppIds(sid) }));
  } catch {
    // 持久化失败不阻断交互
  }
}

/**
 * 进入空间时恢复窗口（每应用单实例）：先清桶再按持久化的 appId 各开一窗。
 * 无持久化记录则为空桌面。仅对当前空间生效（PcDesktop 挂载/切空间时调用）。
 */
export function restorePersistedWindows(spaceId?: string): void {
  const sid = spaceId ?? desktopSpaceId.value;
  if (restoredSpaces.has(sid) || bucket(sid).instances.size > 0) {
    restoredSpaces.add(sid);
    return;
  }
  restoredSpaces.add(sid);
  restoreWindows(loadPersistedOpenApps(sid), sid);
}

const restoredSpaces = new Set<string>();
let persistWatchStarted = false;
/**
 * 启动「窗口变化 → 持久化」监听（幂等）：当前空间窗口集变化即保存。
 * 由 PcDesktop 挂载时启动一次；用 watch 监听 windows 引用变化。
 */
export function startWindowsPersistence(): void {
  if (persistWatchStarted) {
    return;
  }
  persistWatchStarted = true;
  // 延迟 import 避免与 vue 初始化循环（windows 是本模块 computed）
  watch(windowGroups, (groups) => groups.forEach((group) => persistWindows(group.space.id)), { deep: true });
}
