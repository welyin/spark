<!-- PC 桌面任务栏 / Dock（阶段 2 / shell-desktop §3.5(4)）：
     底部居中，两段 = 固定快捷区 ｜ 已开但未固定的窗（运行中图标下小圆点）。
     点击三态（标准 OS 语义）：未开→打开；前台→最小化；后台→置顶激活（toggleWindow）。
     毛玻璃透明度/底色走 --spark-* 令牌；Dock 固定项按空间本机持久化（spark:desktop:<spaceId>）。 -->
<template>
  <div class="task-dock">
    <div class="dock-inner">
      <button type="button" class="dock-item" title="全局搜索" @click="openSearch">
        <el-icon :size="22"><Search /></el-icon>
      </button>
      <!-- 固定快捷区 -->
      <button
        v-for="app in pinnedApps"
        :key="`pin-${app.id}`"
        type="button"
        class="dock-item"
        :class="{ running: isRunning(app.id), foreground: isForeground(app.id) }"
        :title="app.name"
        @click="toggle(app.id)"
      >
        <span class="dock-icon" :style="{ background: hashGradient(app.name) }">{{ app.icon }}</span>
        <span v-if="isRunning(app.id)" class="dock-dot" />
      </button>

      <!-- 分隔线（既有固定又有运行时） -->
      <div v-if="pinnedApps.length > 0 && unpinnedRunning.length > 0" class="dock-divider" />

      <!-- 已开但未固定的窗 -->
      <button
        v-for="w in unpinnedRunning"
        :key="w.key"
        type="button"
        class="dock-item running"
        :class="{ foreground: activeWinKey === w.key }"
        :title="`${titleOf(w.appId)} · 窗口 ${w.seq}`"
        @click="toggleWindow(w.key)"
      >
        <span class="dock-icon" :style="{ background: hashGradient(titleOf(w.appId)) }">{{ iconOf(w.appId) }}</span>
        <span class="dock-dot" />
      </button>

      <!-- 应用市场快捷入口 -->
      <div v-if="pinnedApps.length > 0 || unpinnedRunning.length > 0" class="dock-divider" />
      <button type="button" class="dock-item" title="应用市场" @click="emit('open-market')">
        <span class="dock-icon dock-icon-market"><el-icon :size="20"><Grid /></el-icon></span>
      </button>
      <button type="button" class="dock-item" title="回到桌面" @click="minimizeAllWindows">
        <el-icon :size="22"><Monitor /></el-icon>
      </button>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { Grid, Search, Monitor } from '@element-plus/icons-vue';
import { appRegistry, desktopSpaceId, getApp } from '../../stores/desktop/app-registry';
import { activeWinKey, minimizeAllWindows, openWindow, toggleWindow, windows, type WinInst } from '../../stores/desktop/window-manager';
import { desktopLayout } from '../../stores/desktop/layout';
import { hashGradient } from '../../utils/palette';

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
  components: { Grid, Search, Monitor },
  emits: ['open-market'],
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
    const iconOf = (appId: string) => getApp(appId)?.icon ?? '?';

    /** Dock 三态：未开→打开；已开→toggle（前台最小化/后台置顶） */
    const toggle = (appId: string) => {
      const running = [...windows.value].reverse().find((w) => w.appId === appId);
      if (!running) {
        openWindow(appId);
      } else {
        toggleWindow(running.key);
      }
    };

    return { pinnedApps, unpinnedRunning, activeWinKey, isRunning, isForeground, openSearch, minimizeAllWindows, titleOf, iconOf, toggle, toggleWindow, hashGradient, Grid, emit };
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

.dock-item.foreground {
  background: var(--spark-primary-light);
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

.dock-icon-market {
  background: var(--spark-bg-hover);
  color: var(--spark-text-2);
}

.dock-dot {
  position: absolute;
  bottom: 2px;
  left: 50%;
  transform: translateX(-50%);
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: var(--spark-primary);
}

.dock-divider {
  width: 1px;
  height: 28px;
  background: var(--spark-border-light);
  margin: 0 2px;
}
</style>
