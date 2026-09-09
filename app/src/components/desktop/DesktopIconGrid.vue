<!-- PC 空间桌面图标网格（阶段 2 / shell-desktop §3.3）：
     当前域已装应用的桌面图标，单击选中、双击打开窗口、右键菜单（打开/从桌面移除——
     卸载进应用市场）。图标排布按空间本机持久化（spark:desktop:<spaceId>，2.5 简化：有序列表）。 -->
<template>
  <div ref="gridEl" class="pc-icon-grid" @contextmenu.self.prevent="onBlankContext" @click.self="selectedId = null">
    <el-alert v-if="loadError" :title="loadError" type="error" :closable="false" show-icon />
    <!-- 隐藏文件选择：更换壁纸用（本地图片读为 dataURL，本机按空间存储） -->
    <input ref="wallpaperInput" type="file" accept="image/*" style="display:none" @change="onWallpaperFile" />
    <button
      v-for="app in orderedApps"
      :key="app.id"
      type="button"
      class="pc-icon"
      :title="app.name"
      :class="{ selected: selectedId === app.id, dragging: draggingId === app.id }"
      :style="iconStyle(app.id)"
      @click="selectedId = app.id"
      @dblclick="open(app.id)"
      @keydown.enter.prevent="open(app.id)"
      @contextmenu.prevent.stop="onIconContext(app.id, $event)"
      @pointerdown="onIconPointerDown(app.id, $event)"
    >
      <span class="pc-icon-badge" :style="{ background: hashGradient(app.name) }">{{ app.icon }}</span>
      <span class="pc-icon-name">{{ app.name }}</span>
    </button>

    <div v-if="orderedApps.length === 0" class="pc-grid-empty">
      <el-empty :image-size="90" description="这张桌面还没有应用">
        <el-button type="primary" @click="emit('open-market')">去应用市场</el-button>
      </el-empty>
      <el-button v-if="loadError" @click="refreshAppRegistry">重新加载</el-button>
    </div>

    <!-- 桌面空白处右键菜单（参照 ark-desktop-main：全局 fixed 弹层定位到光标处，
         边界翻转 + 遮罩点击关闭；不依赖 el-dropdown 的锚点定位） -->
    <Teleport to="body">
      <div
        v-if="blankMenu.visible"
        class="pc-blank-menu-mask"
        @click="closeBlankMenu"
        @contextmenu.prevent="closeBlankMenu"
      >
        <div
          class="pc-blank-menu"
          :style="{ left: `${blankMenu.x}px`, top: `${blankMenu.y}px` }"
          @click.stop
        >
          <button type="button" class="pc-blank-menu-item" @click="onBlankCommand('wallpaper')">
            更换壁纸（本地图片）
          </button>
          <button
            type="button"
            class="pc-blank-menu-item"
            :disabled="!desktopLayout.background"
            @click="onBlankCommand('reset-wallpaper')"
          >
            恢复默认背景
          </button>
          <button
            type="button"
            class="pc-blank-menu-item"
            :disabled="!desktopLayout.positions || Object.keys(desktopLayout.positions).length === 0"
            @click="onBlankCommand('arrange')"
          >
            排列图标（重置为网格）
          </button>
        </div>
      </div>
    </Teleport>
    <!-- 图标右键菜单（同一光标定位弹层） -->
    <Teleport to="body">
      <div
        v-if="iconMenu.visible"
        class="pc-blank-menu-mask"
        @click="closeIconMenu"
        @contextmenu.prevent="closeIconMenu"
      >
        <div
          class="pc-blank-menu"
          :style="{ left: `${iconMenu.x}px`, top: `${iconMenu.y}px` }"
          @click.stop
        >
          <button type="button" class="pc-blank-menu-item" @click="onCommand(iconMenu.appId, 'open')">打开</button>
          <button type="button" class="pc-blank-menu-item" @click="onCommand(iconMenu.appId, 'new')">新窗口打开</button>
          <button type="button" class="pc-blank-menu-item" @click="onCommand(iconMenu.appId, 'pin')">
            {{ desktopLayout.dock.includes(iconMenu.appId) ? '取消固定到任务栏' : '固定到任务栏' }}
          </button>
          <button type="button" class="pc-blank-menu-item" @click="onCommand(iconMenu.appId, 'market')">信任、权限与卸载</button>
        </div>
      </div>
    </Teleport>

  </div>
</template>

<script lang="ts">
import { computed, defineComponent, reactive, ref } from 'vue';
import { appList, appRegistryError, refreshAppRegistry } from '../../stores/desktop/app-registry';
import { openNewWindow, openWindow } from '../../stores/desktop/window-manager';
import { hashGradient } from '../../utils/palette';
import { desktopLayout, clearIconPositions, saveIconPosition, saveWallpaper, togglePinned } from '../../stores/desktop/layout';
import { requestOpenAppDetail } from '../../stores/pending-app';

export default defineComponent({
  name: 'DesktopIconGrid',
  emits: ['open-market'],
  setup(_, { emit }) {
    const selectedId = ref<string | null>(null);
    const orderedApps = computed(() => {
      const order = new Map(desktopLayout.value.apps.map((id, index) => [id, index]));
      return [...appList.value].sort((first, second) => (order.get(first.id) ?? Infinity) - (order.get(second.id) ?? Infinity));
    });
    const loadError = appRegistryError;

    /* ---- 图标自由摆放（Pointer 拖动到任意位置，位置按空间持久化） ----
       参照桌面 OS 语义：图标不是网格顺序，而是 (x,y) 绝对坐标；
       未自定义的图标按网格自动排（列优先，84px 宽 × 110px 高间距）。 */
    const gridEl = ref<HTMLElement | null>(null);
    const draggingId = ref<string | null>(null);
    const dragPos = reactive({ x: 0, y: 0 });
    const COL_W = 84;
    const ROW_H = 110;
    const PAD = 12;
    /** 吸附到最近网格位（用户评审：要网格吸附而非纯自由坐标） */
    const snapToGrid = (pos: { x: number; y: number }) => ({
      x: PAD + Math.round((pos.x - PAD) / COL_W) * COL_W,
      y: PAD + Math.round((pos.y - PAD) / ROW_H) * ROW_H
    });
    const defaultPos = (index: number) => {
      const cols = Math.max(1, Math.floor((gridEl.value?.clientWidth ?? 800 - PAD * 2) / COL_W));
      return { x: PAD + (index % cols) * COL_W, y: PAD + Math.floor(index / cols) * ROW_H };
    };
    const iconStyle = (appId: string) => {
      const custom = desktopLayout.value.positions?.[appId];
      const index = orderedApps.value.findIndex((a) => a.id === appId);
      const pos = draggingId.value === appId ? dragPos : custom ?? defaultPos(Math.max(0, index));
      return { left: `${pos.x}px`, top: `${pos.y}px` };
    };
    const onIconPointerDown = (appId: string, e: PointerEvent) => {
      if (e.button !== 0) return;
      selectedId.value = appId;
      const container = gridEl.value;
      if (!container) return;
      const rect = container.getBoundingClientRect();
      const startX = e.clientX;
      const startY = e.clientY;
      const custom = desktopLayout.value.positions?.[appId];
      const index = orderedApps.value.findIndex((a) => a.id === appId);
      const orig = custom ?? defaultPos(Math.max(0, index));
      let moved = false;
      const onMove = (ev: PointerEvent) => {
        const dx = ev.clientX - startX;
        const dy = ev.clientY - startY;
        if (!moved && Math.abs(dx) + Math.abs(dy) < 4) return;
        moved = true;
        draggingId.value = appId;
        // 拖动中实时吸附到最近网格位，再夹取到容器内
        const snapped = snapToGrid({ x: orig.x + dx, y: orig.y + dy });
        dragPos.x = Math.max(PAD, Math.min(snapped.x, rect.width - COL_W));
        dragPos.y = Math.max(PAD, Math.min(snapped.y, rect.height - ROW_H));
      };
      const onUp = () => {
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        if (moved) {
          saveIconPosition(appId, { x: dragPos.x, y: dragPos.y });
        }
        draggingId.value = null;
      };
      document.addEventListener('pointermove', onMove);
      document.addEventListener('pointerup', onUp);
    };

    const open = (appId: string) => openWindow(appId);
    const openNew = (appId: string) => openNewWindow(appId);

    // 桌面空白处右键菜单（参照 ark：光标定位 + 边界翻转 + 遮罩关闭）
    const wallpaperInput = ref<HTMLInputElement | null>(null);
    const blankMenu = reactive({ visible: false, x: 0, y: 0 });
    const iconMenu = reactive({ visible: false, x: 0, y: 0, appId: '' });
    const MENU_W = 208;
    const MENU_H = 96;
    const ICON_MENU_H = 176;
    const onBlankContext = (e: MouseEvent) => {
      // 空白处右键：清图标选中，菜单弹出在光标处（越界时向内翻）
      selectedId.value = null;
      closeIconMenu();
      blankMenu.x = Math.min(e.clientX, window.innerWidth - MENU_W - 8);
      blankMenu.y = Math.min(e.clientY, window.innerHeight - MENU_H - 8);
      blankMenu.visible = true;
    };
    const closeBlankMenu = () => {
      blankMenu.visible = false;
    };
    const onIconContext = (appId: string, e: MouseEvent) => {
      // 图标右键：选中该图标，菜单弹出在光标处
      selectedId.value = appId;
      closeBlankMenu();
      iconMenu.appId = appId;
      iconMenu.x = Math.min(e.clientX, window.innerWidth - MENU_W - 8);
      iconMenu.y = Math.min(e.clientY, window.innerHeight - ICON_MENU_H - 8);
      iconMenu.visible = true;
    };
    const closeIconMenu = () => {
      iconMenu.visible = false;
    };
    const onBlankCommand = (command: string) => {
      closeBlankMenu();
      if (command === 'wallpaper') {
        wallpaperInput.value?.click();
      } else if (command === 'reset-wallpaper') {
        saveWallpaper(null);
      } else if (command === 'arrange') {
        clearIconPositions();
      }
    };
    const onWallpaperFile = (event: Event) => {
      const file = (event.target as HTMLInputElement).files?.[0];
      (event.target as HTMLInputElement).value = '';
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => saveWallpaper(String(reader.result ?? ''));
      reader.readAsDataURL(file);
    };
    const onCommand = (appId: string, command: string) => {
      closeIconMenu();
      if (command === 'open') open(appId);
      else if (command === 'new') openNew(appId);
      else if (command === 'pin') togglePinned(appId);
      else if (command === 'market') {
        requestOpenAppDetail(appId);
        emit('open-market');
      }
    };

    return {
      selectedId,
      orderedApps,
      loadError,
      refreshAppRegistry,
      desktopLayout,
      gridEl,
      draggingId,
      iconStyle,
      onIconPointerDown,
      wallpaperInput,
      blankMenu,
      iconMenu,
      onBlankContext,
      closeBlankMenu,
      onIconContext,
      closeIconMenu,
      onBlankCommand,
      onWallpaperFile,
      onCommand,
      hashGradient,
      open,
      emit
    };
  }
});
</script>

<style scoped>
.pc-icon-grid {
  position: absolute;
  inset: 0;
  padding: var(--spark-padding-page);
  overflow-y: auto;
}

/* 图标自由摆放：绝对定位，Pointer 拖到任意位置 */
.pc-icon {
  position: absolute;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  width: var(--spark-desktop-icon-size);
  padding: 8px 4px;
  border: 0;
  border-radius: var(--spark-radius-l);
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  touch-action: none;
  user-select: none;
  transition: left 0.15s ease, top 0.15s ease;
}

.pc-icon.dragging {
  transition: none;
  cursor: grabbing;
  z-index: 2;
}

.pc-icon:hover {
  background: var(--spark-bg-hover);
}

.pc-icon.selected {
  background: var(--spark-primary-light);
}

.pc-icon-badge {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  border-radius: var(--spark-radius-l);
  color: var(--spark-text-on-color);
  font-size: 20px;
  font-weight: 600;
  box-shadow: var(--spark-shadow-card);
}

.pc-icon-name {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-1);
  max-width: 100%;
  white-space: normal;
  line-height: 18px;
  overflow-wrap: anywhere;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  text-overflow: ellipsis;
}

.pc-grid-empty {
  width: 100%;
  padding: 60px 0;
}

.pc-grid-debug {
  margin: 8px 0 0;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

/* 桌面空白右键菜单（ark 模式）：全屏遮罩点击关闭 + fixed 浮层 */
.pc-blank-menu-mask {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
}

.pc-blank-menu {
  position: fixed;
  min-width: 200px;
  padding: 4px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  box-shadow: var(--spark-shadow-pop);
  display: flex;
  flex-direction: column;
}

.pc-blank-menu-item {
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

.pc-blank-menu-item:hover:not(:disabled) {
  background: var(--spark-bg-hover);
}

.pc-blank-menu-item:disabled {
  color: var(--spark-text-3);
  cursor: default;
}

.pc-ctx-mask {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
}

.pc-ctx-menu {
  position: fixed;
  min-width: 160px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  box-shadow: var(--spark-shadow-pop);
  padding: 4px;
  display: flex;
  flex-direction: column;
}

.pc-ctx-item {
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

.pc-ctx-item:hover {
  background: var(--spark-bg-hover);
}
</style>
