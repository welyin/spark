<!-- PC 桌面任务栏 / Dock（阶段 2 / shell-desktop §3.5(4)）：
     底部居中，两段 = 固定快捷区 ｜ 已开但未固定的窗（运行中图标下小圆点）。
     工具区四项齐：全局搜索、应用市场、回到桌面、九宫格（D4 所有应用 Launchpad 铺层开关）。
     点击三态（标准 OS 语义）：未开→打开；前台→最小化；后台→置顶激活（toggleWindow）。
     W3：图标右键菜单（固定/取消固定、从桌面移除——进应用市场管理），悬停 tooltip 用 Element el-tooltip；
     data-dock-app 供 W6 开关窗动效锚定 Dock 图标位置。
     毛玻璃透明度/底色走 --spark-* 令牌；Dock 固定项按空间本机持久化（spark:desktop:<spaceId>）。 -->
<template>
  <div class="task-dock">
    <div class="dock-inner">
      <el-tooltip content="全局搜索" placement="top" :show-after="300">
        <button type="button" class="dock-item" @click="openSearch">
          <el-icon :size="22"><Search /></el-icon>
        </button>
      </el-tooltip>
      <!-- 固定快捷区 -->
      <el-tooltip v-for="app in pinnedApps" :key="`pin-${app.id}`" :content="app.name" placement="top" :show-after="300">
        <button
          type="button"
          class="dock-item"
          :class="{ running: isRunning(app.id), foreground: isForeground(app.id) }"
          :data-dock-app="app.id"
          @click="toggle(app.id)"
          @contextmenu.prevent.stop="onDockContext(app.id, true, $event)"
        >
          <AppIcon class="dock-icon" :item="iconItemOf(app.id)" />
          <span v-if="isRunning(app.id)" class="dock-dot" />
        </button>
      </el-tooltip>

      <!-- 分隔线（既有固定又有运行时） -->
      <div v-if="pinnedApps.length > 0 && unpinnedRunning.length > 0" class="dock-divider" />

      <!-- 已开但未固定的窗 -->
      <el-tooltip
        v-for="w in unpinnedRunning"
        :key="w.key"
        :content="`${titleOf(w.appId)} · 窗口 ${w.seq}`"
        placement="top"
        :show-after="300"
      >
        <button
          type="button"
          class="dock-item running"
          :class="{ foreground: activeWinKey === w.key }"
          :data-dock-app="w.appId"
          @click="toggleWindow(w.key)"
          @contextmenu.prevent.stop="onDockContext(w.appId, false, $event)"
        >
          <AppIcon class="dock-icon" :item="iconItemOf(w.appId)" />
          <span class="dock-dot" />
        </button>
      </el-tooltip>

      <!-- 工具区：应用市场 / 回到桌面 / 九宫格（所有应用 Launchpad，D4）；
           四个工具项同为裸线图标（走查修正：应用市场不再用色块图标，与搜索/九宫格统一） -->
      <div v-if="pinnedApps.length > 0 || unpinnedRunning.length > 0" class="dock-divider" />
      <el-tooltip content="应用市场" placement="top" :show-after="300">
        <button type="button" class="dock-item" @click="openWindow('spark:space-market')">
          <el-icon :size="22"><Shop /></el-icon>
        </button>
      </el-tooltip>
      <el-tooltip content="回到桌面" placement="top" :show-after="300">
        <button type="button" class="dock-item" @click="minimizeAllWindows">
          <el-icon :size="22"><Monitor /></el-icon>
        </button>
      </el-tooltip>
      <el-tooltip content="所有应用" placement="top" :show-after="300">
        <button
          type="button"
          class="dock-item"
          :class="{ foreground: launchpadOpen }"
          @click="emit('toggle-launchpad')"
        >
          <el-icon :size="22"><Grid /></el-icon>
        </button>
      </el-tooltip>
    </div>

    <!-- W3 Dock 图标右键菜单（与桌面右键同一弹层模式：光标定位 + 越界翻转 + 遮罩关闭） -->
    <Teleport to="body">
      <div
        v-if="ctxMenu.visible"
        class="dock-ctx-mask"
        @click="closeCtxMenu"
        @contextmenu.prevent="closeCtxMenu"
      >
        <div class="dock-ctx-menu" :style="{ left: `${ctxMenu.x}px`, top: `${ctxMenu.y}px` }" @click.stop>
          <button type="button" class="dock-ctx-item" @click="onCtxCommand('pin')">
            {{ ctxMenu.pinned ? '取消固定' : '固定到任务栏' }}
          </button>
          <button type="button" class="dock-ctx-item" @click="onCtxCommand('remove')">从桌面移除</button>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, reactive } from 'vue';
import { Grid, Search, Monitor, Shop } from '@element-plus/icons-vue';
import { appRegistry, desktopSpaceId, getApp, getMarketItem } from '../../stores/desktop/app-registry';
import { activeWinKey, minimizeAllWindows, openWindow, toggleWindow, windows, type WinInst } from '../../stores/desktop/window-manager';
import { desktopLayout, togglePinned } from '../../stores/desktop/layout';
import { requestOpenAppDetail } from '../../stores/pending-app';
import { openShellModal } from '../../stores/shell-modal';
import AppIcon from '../apps/AppIcon.vue';
import type { AppIconItem } from '../apps/app-icon';

function loadPinned(spaceId: string): string[] {
  try {
    const raw = localStorage.getItem(`spark:desktop:${spaceId}`);
    if (raw) {
      const parsed = JSON.parse(raw) as { dock?: string[] };
      return parsed.dock ?? [];
    }
  } catch {
    // 忽略损坏数据
  }
  return [];
}

export default defineComponent({
  name: 'TaskDock',
  components: { Grid, Search, Monitor, Shop, AppIcon },
  emits: ['toggle-launchpad'],
  props: {
    /** D4 所有应用 Launchpad 是否铺开（九宫格图标高亮） */
    launchpadOpen: { type: Boolean, default: false }
  },
  setup(_, { emit }) {
    /** 固定快捷区（按空间本机持久化的 dock 列表 → AppDef，过滤已不可用项） */
    const pinnedApps = computed(() =>
      desktopLayout.value.dock
        .map((id) => appRegistry.value.get(id))
        .filter((a): a is NonNullable<typeof a> => Boolean(a))
    );

    /** 已开但未固定的窗口（按 appId 去重，Dock 一应用一图标） */
    const unpinnedRunning = computed<WinInst[]>(() => {
      const pinnedIds = new Set(pinnedApps.value.map((a) => a.id));
      const out: WinInst[] = [];
      for (const w of windows.value) {
        if (pinnedIds.has(w.appId) && windows.value.filter((entry) => entry.appId === w.appId).length === 1) {
          continue;
        }
        out.push(w);
      }
      return out;
    });

    const isRunning = (appId: string) => windows.value.some((w) => w.appId === appId);
    const isForeground = (appId: string) => windows.value.some((w) => w.appId === appId && w.key === activeWinKey.value);
    const openSearch = () => window.dispatchEvent(new Event('spark:search'));
    const titleOf = (appId: string) => getApp(appId)?.name ?? appId;
    /** 图标条目：优先市场条目（含声明图标/安装态），壳层内置窗口（spark:*）等
        无市场条目的退化为最小形状走首字符回退（plugin-dist §2.3） */
    const iconItemOf = (appId: string): AppIconItem =>
      getMarketItem(appId) ?? { id: appId, name: titleOf(appId) };

    /** Dock 三态：未开→打开；已开→toggle（前台最小化/后台置顶） */
    const toggle = (appId: string) => {
      const running = [...windows.value].reverse().find((w) => w.appId === appId);
      if (!running) {
        openWindow(appId);
      } else {
        toggleWindow(running.key);
      }
    };

    /* ---- W3 Dock 图标右键菜单：固定/取消固定、从桌面移除（进应用市场管理/卸载） ---- */
    const ctxMenu = reactive({ visible: false, x: 0, y: 0, appId: '', pinned: false });
    const CTX_MENU_W = 160;
    const CTX_MENU_H = 2 * 36 + 8;
    const onDockContext = (appId: string, pinned: boolean, e: MouseEvent) => {
      ctxMenu.appId = appId;
      ctxMenu.pinned = pinned;
      ctxMenu.x = Math.max(8, Math.min(e.clientX, window.innerWidth - CTX_MENU_W - 8));
      ctxMenu.y = Math.max(8, Math.min(e.clientY, window.innerHeight - CTX_MENU_H - 8));
      ctxMenu.visible = true;
    };
    const closeCtxMenu = () => {
      ctxMenu.visible = false;
    };
    const onCtxCommand = (command: 'pin' | 'remove') => {
      const appId = ctxMenu.appId;
      closeCtxMenu();
      if (command === 'pin') {
        togglePinned(appId);
      } else {
        // 从桌面移除＝进系统层应用管理的详情（装卸只在那里；空间市场窗口只管启停）
        requestOpenAppDetail(appId);
        openShellModal('apps');
      }
    };

    return { pinnedApps, unpinnedRunning, activeWinKey, isRunning, isForeground, openSearch, openWindow, minimizeAllWindows, titleOf, iconItemOf, toggle, toggleWindow, Grid, ctxMenu, onDockContext, closeCtxMenu, onCtxCommand, emit };
  }
});
</script>

<style scoped>
.task-dock {
  position: absolute;
  left: 50%;
  bottom: 12px;
  transform: translateX(-50%);
  z-index: var(--spark-z-nav);
  pointer-events: none;
  max-width: calc(100% - 24px);
}

.dock-inner {
  pointer-events: auto;
  display: flex;
  align-items: center;
  gap: 6px;
  min-height: var(--spark-dock-height);
  padding: 6px 10px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-xl);
  box-shadow: var(--spark-shadow-pop);
  opacity: 0.96;
  overflow-x: auto;
  backdrop-filter: blur(18px);
}

.dock-item {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 44px;
  flex: 0 0 44px;
  color: var(--spark-text-1);
  border: 0;
  border-radius: var(--spark-radius-l);
  background: transparent;
  cursor: pointer;
  transition: transform var(--spark-dur-fast) var(--spark-ease-standard), background var(--spark-dur-fast) var(--spark-ease-standard);
}

.dock-item:hover {
  background: var(--spark-bg-hover);
  transform: translateY(-2px);
}

/* D16：前台高亮与运行圆点取本空间主题色（--spark-space-accent 由桌面层按空间注入，缺省回退品牌色） */
.dock-item.foreground {
  background: color-mix(in srgb, var(--spark-space-accent, var(--spark-primary)) 14%, transparent);
}

.dock-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  border-radius: var(--spark-radius-m);
  color: var(--spark-text-on-color);
  font-size: 15px;
  font-weight: 600;
}

.dock-dot {
  position: absolute;
  bottom: 2px;
  left: 50%;
  transform: translateX(-50%);
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: var(--spark-space-accent, var(--spark-primary));
}

.dock-divider {
  width: 1px;
  height: 28px;
  background: var(--spark-border-light);
  margin: 0 2px;
}

/* W3 Dock 右键菜单：全屏遮罩点击关闭 + fixed 浮层（与桌面右键菜单同模式） */
.dock-ctx-mask {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
}

.dock-ctx-menu {
  position: fixed;
  min-width: 150px;
  padding: 4px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  box-shadow: var(--spark-shadow-pop);
  display: flex;
  flex-direction: column;
}

.dock-ctx-item {
  padding: 8px 12px;
  border: 0;
  border-radius: var(--spark-radius-s);
  background: transparent;
  text-align: left;
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
  cursor: pointer;
  font-family: inherit;
}

.dock-ctx-item:hover {
  background: var(--spark-bg-hover);
}
</style>
