import { computed, reactive } from 'vue';
import { desktopSpaceId } from './app-registry';

/** D6 桌面图标排列方式：网格排列（忽略自由坐标、按网格流式排布）/ 自由放置（绝对坐标）。
    属本机偏好，按空间本地记忆（同 wallpaper/positions 机制），不跨端同步。 */
export type ArrangeMode = 'grid' | 'free';

type DesktopLayout = {
  dock: string[];
  apps: string[];
  background?: string;
  /** 图标自由摆放位置（px，相对桌面容器）：appId → { x, y } */
  positions?: Record<string, { x: number; y: number }>;
  /** D6 排列方式；缺省 'free'（保持现状绝对坐标语义），切网格不丢自由坐标 */
  arrange?: ArrangeMode;
  /** D16 每空间主题色（CSS 颜色值；缺省按 spaceId 哈希派生，本机存储不跨端） */
  themeColor?: string;
};
const layouts = reactive(new Map<string, DesktopLayout>());

export const desktopLayout = computed(() => {
  const spaceId = desktopSpaceId.value;
  if (!layouts.has(spaceId)) {
    let layout: DesktopLayout = { dock: [], apps: [], arrange: 'free' };
    try {
      const stored = JSON.parse(localStorage.getItem(`spark:desktop:${spaceId}`) ?? '{}');
      layout = {
        dock: Array.isArray(stored.dock) ? stored.dock.filter((id: unknown) => typeof id === 'string') : [],
        apps: Array.isArray(stored.apps) ? stored.apps.filter((id: unknown) => typeof id === 'string') : [],
        background: typeof stored.background === 'string' ? stored.background : undefined,
        positions: stored.positions && typeof stored.positions === 'object' ? stored.positions : {},
        arrange: stored.arrange === 'grid' || stored.arrange === 'free' ? stored.arrange : 'free',
        themeColor: typeof stored.themeColor === 'string' ? stored.themeColor : undefined
      };
    } catch {}
    layouts.set(spaceId, layout);
  }
  return layouts.get(spaceId)!;
});

/** 当前空间排列方式（缺省 'free'） */
export const arrangeMode = computed<ArrangeMode>(() => desktopLayout.value.arrange ?? 'free');

function persist() {
  try {
    localStorage.setItem(`spark:desktop:${desktopSpaceId.value}`, JSON.stringify(desktopLayout.value));
  } catch {}
}

export function togglePinned(appId: string) {
  const layout = desktopLayout.value;
  layout.dock = layout.dock.includes(appId) ? layout.dock.filter((id) => id !== appId) : [...layout.dock, appId];
  persist();
}

export function saveAppOrder(appIds: string[]) {
  desktopLayout.value.apps = [...appIds];
  persist();
}

/** 桌面壁纸（按空间本机）：存 dataURL；传 null 恢复默认高级灰 */
export function saveWallpaper(dataUrl: string | null) {
  const layout = desktopLayout.value;
  if (dataUrl) {
    layout.background = dataUrl;
  } else {
    delete layout.background;
  }
  persist();
}

/** 图标自由摆放：写某应用在当前空间的桌面坐标（px，相对容器） */
export function saveIconPosition(appId: string, pos: { x: number; y: number }) {
  const layout = desktopLayout.value;
  if (!layout.positions) {
    layout.positions = {};
  }
  layout.positions[appId] = { x: Math.round(pos.x), y: Math.round(pos.y) };
  persist();
}

/** 清除全部自由摆放位置（恢复网格排列） */
export function clearIconPositions() {
  const layout = desktopLayout.value;
  if (layout.positions && Object.keys(layout.positions).length > 0) {
    layout.positions = {};
    persist();
  }
}

/** D6 切换排列方式（本机偏好，按空间本地记忆；自由坐标保留，切回不丢） */
export function setArrangeMode(mode: ArrangeMode) {
  const layout = desktopLayout.value;
  if (layout.arrange !== mode) {
    layout.arrange = mode;
    persist();
  }
}

/** D16 每空间主题色（按空间本机存储）：写 CSS 颜色值；传 null 恢复按 spaceId 派生的默认色 */
export function saveThemeColor(color: string | null) {
  const layout = desktopLayout.value;
  if (color) {
    layout.themeColor = color;
  } else {
    delete layout.themeColor;
  }
  persist();
}