import { computed, reactive } from 'vue';
import { desktopSpaceId } from './app-registry';

type DesktopLayout = {
  dock: string[];
  apps: string[];
  background?: string;
  /** 图标自由摆放位置（px，相对桌面容器）：appId → { x, y } */
  positions?: Record<string, { x: number; y: number }>;
};
const layouts = reactive(new Map<string, DesktopLayout>());

export const desktopLayout = computed(() => {
  const spaceId = desktopSpaceId.value;
  if (!layouts.has(spaceId)) {
    let layout: DesktopLayout = { dock: [], apps: [] };
    try {
      const stored = JSON.parse(localStorage.getItem(`spark:desktop:${spaceId}`) ?? '{}');
      layout = {
        dock: Array.isArray(stored.dock) ? stored.dock.filter((id: unknown) => typeof id === 'string') : [],
        apps: Array.isArray(stored.apps) ? stored.apps.filter((id: unknown) => typeof id === 'string') : [],
        background: typeof stored.background === 'string' ? stored.background : undefined,
        positions: stored.positions && typeof stored.positions === 'object' ? stored.positions : {}
      };
    } catch {}
    layouts.set(spaceId, layout);
  }
  return layouts.get(spaceId)!;
});

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