<!-- PC 空间桌面图标网格（阶段 2 / shell-desktop §3.3）：
     当前域已装应用的桌面图标，单击选中、双击打开窗口、右键菜单（打开/从桌面移除——
     卸载进应用市场）。图标排布按空间本机持久化（spark:desktop:<spaceId>）；
     D6：空白处右键可切换 网格排列（忽略自由坐标、拖动吸附重排）/ 自由放置（绝对坐标）。 -->
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
      <AppIcon class="pc-icon-badge" :item="iconItemOf(app)" />
      <span class="pc-icon-name">{{ app.name }}</span>
    </button>

    <!-- D15 空桌面 / 首次进入引导：「这是什么 + 下一步」，不留空白页 -->
    <div v-if="orderedApps.length === 0" class="pc-grid-empty">
      <div class="pc-guide">
        <div class="pc-guide-title">这是「{{ spaceName }}」的桌面</div>
        <p class="pc-guide-desc">
          桌面是你在这个空间的工作区：把应用图标放在这里、像电脑桌面一样开多个窗口并排处理事务。
          这个空间还没有已启用的应用，从下面任选一步开始。
        </p>
        <div class="pc-guide-actions">
          <!-- 桌面上下文入口＝空间的应用市场（install-and-enable §一②/§五③：先启用后安装——
               启用未安装的应用会先自动装到本机；装卸在系统层应用管理） -->
          <button type="button" class="pc-guide-card" @click="emit('open-enable-apps')">
            <span class="pc-guide-card-title">为本空间启用应用</span>
            <span class="pc-guide-card-desc">在空间的应用市场里启用，未安装的会先自动装到本机</span>
          </button>
          <button type="button" class="pc-guide-card" @click="openAffairs">
            <span class="pc-guide-card-title">发起第一个事务</span>
            <span class="pc-guide-card-desc">打开事务列表，看看有没有等你处理的事</span>
          </button>
        </div>
      </div>
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
          <!-- D6 排列方式切换（本机偏好、按空间本地记忆）：网格排列 / 自由放置 -->
          <button type="button" class="pc-blank-menu-item" @click="onBlankCommand('arrange-grid')">
            {{ arrangeMode === 'grid' ? '✓ 网格排列' : '网格排列' }}
          </button>
          <button type="button" class="pc-blank-menu-item" @click="onBlankCommand('arrange-free')">
            {{ arrangeMode === 'free' ? '✓ 自由放置' : '自由放置' }}
          </button>
          <button
            type="button"
            class="pc-blank-menu-item"
            :disabled="arrangeMode === 'grid' || !desktopLayout.positions || Object.keys(desktopLayout.positions).length === 0"
            @click="onBlankCommand('arrange')"
          >
            排列图标（重置为网格）
          </button>
        </div>
      </div>
    </Teleport>
    <!-- 图标右键菜单：与 Launchpad 共用 AppIconMenu（打开/新窗口/固定/复制链接/属性；
         装卸不在此——系统层应用管理；启用/停用在属性窗口） -->
    <AppIconMenu
      :visible="iconMenu.visible"
      :x="iconMenu.x"
      :y="iconMenu.y"
      :app-id="iconMenu.appId"
      :space="sourceSpaceOf()"
      @close="closeIconMenu"
      @open-props="(id) => emit('open-props', id)"
    />

  </div>
</template>

<script lang="ts">
import { computed, defineComponent, reactive, ref } from 'vue';
import { appList, appRegistryError, getMarketItem, refreshAppRegistry } from '../../stores/desktop/app-registry';
import { openWindow } from '../../stores/desktop/window-manager';
import { desktopLayout, arrangeMode, clearIconPositions, saveAppOrder, saveIconPosition, saveWallpaper, setArrangeMode } from '../../stores/desktop/layout';
import { openShellModal } from '../../stores/shell-modal';
import { currentSpace, currentSpaceOrgId } from '../../stores/current-space';
import { personalSpaceName } from '../../stores/personal-space';
import { findOrg } from '../../stores/org-membership';
import AppIconMenu from './AppIconMenu.vue';
import AppIcon from '../apps/AppIcon.vue';
import type { AppIconItem } from '../apps/app-icon';
import {
  beginObjectDrag,
  endObjectDrag,
  requestDrop,
  sameSpace,
  updateObjectDrag,
  type SpaceRef
} from '../../stores/cross-domain-drop';
import { spaceRefFromAttr } from '../../stores/os-file-drop';

export default defineComponent({
  name: 'DesktopIconGrid',
  components: { AppIconMenu, AppIcon },
  emits: ['open-enable-apps', 'open-props'],
  setup(_, { emit }) {
    const selectedId = ref<string | null>(null);
    const orderedApps = computed(() => {
      const order = new Map(desktopLayout.value.apps.map((id, index) => [id, index]));
      return [...appList.value].sort((first, second) => (order.get(first.id) ?? Infinity) - (order.get(second.id) ?? Infinity));
    });
    const loadError = appRegistryError;

    /** 图标条目：优先市场条目（含声明图标/安装态，plugin-dist §2.3 回退链），
        无条目（壳层内置窗口等）退化为最小形状 */
    const iconItemOf = (app: { id: string; name: string }): AppIconItem =>
      getMarketItem(app.id) ?? { id: app.id, name: app.name };

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
      const index = orderedApps.value.findIndex((a) => a.id === appId);
      // D6：网格排列 = 忽略自由坐标、按网格流式排布；自由放置 = 绝对坐标（缺省回网格位）
      const custom = arrangeMode.value === 'free' ? desktopLayout.value.positions?.[appId] : undefined;
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
      let dropSessionStarted = false;
      /** X1/X4 拖拽落点 hit-test：指针下的空间导航项（[data-drop-space]）= 该空间（落点即域）；
          其余位置 = 本域（桌面图标重排，不弹确认） */
      const hitSpace = (ev: PointerEvent): SpaceRef | null => {
        const hit = document.elementFromPoint(ev.clientX, ev.clientY);
        const attr = hit?.closest?.('[data-drop-space]')?.getAttribute('data-drop-space') ?? null;
        return attr ? spaceRefFromAttr(attr) : null;
      };
      const onMove = (ev: PointerEvent) => {
        const dx = ev.clientX - startX;
        const dy = ev.clientY - startY;
        if (!moved && Math.abs(dx) + Math.abs(dy) < 4) return;
        moved = true;
        draggingId.value = appId;
        // X1/X4：越过阈值即进入拖拽会话，光标旁持续显示「进哪个域、谁可见」（DropFeedbackHint）
        const hover = hitSpace(ev) ?? sourceSpaceOf();
        if (!dropSessionStarted) {
          dropSessionStarted = true;
          beginObjectDrag({ kind: 'app-icon', id: appId, label: appNameOf(appId), source: sourceSpaceOf() }, ev.clientX, ev.clientY);
        }
        updateObjectDrag(ev.clientX, ev.clientY, hover, true);
        // 拖动中实时吸附到最近网格位，再夹取到容器内
        const snapped = snapToGrid({ x: orig.x + dx, y: orig.y + dy });
        dragPos.x = Math.max(PAD, Math.min(snapped.x, rect.width - COL_W));
        dragPos.y = Math.max(PAD, Math.min(snapped.y, rect.height - ROW_H));
      };
      const onUp = (ev: PointerEvent) => {
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        if (moved) {
          // 松手落在另一空间项：跨域判定（同空间不弹、跨域弹确认）；落在本域桌面：照常重排
          const dropSpace = hitSpace(ev);
          if (dropSpace && !sameSpace(dropSpace, sourceSpaceOf())) {
            requestDrop(dropSpace);
          } else {
            endObjectDrag();
            if (arrangeMode.value === 'grid') {
              // 网格排列：拖动即吸附 —— 按落点网格槽位重排顺序（不写自由坐标）
              const cols = Math.max(1, Math.floor((rect.width - PAD * 2) / COL_W));
              const target = Math.max(
                0,
                Math.round((dragPos.y - PAD) / ROW_H) * cols + Math.round((dragPos.x - PAD) / COL_W)
              );
              const ids = orderedApps.value.map((a) => a.id);
              const from = ids.indexOf(appId);
              if (from >= 0) {
                ids.splice(from, 1);
                ids.splice(Math.min(target, ids.length), 0, appId);
                saveAppOrder(ids);
              }
            } else {
              // 自由放置：落点网格槽位已被其他图标占用则退回原位（不允许同位覆盖）；
              // 占用判定按其他图标的有效位置（自由坐标 ?? 默认网格位）与吸附后落点比较
              const target = { x: dragPos.x, y: dragPos.y };
              const occupied = orderedApps.value.some((app, index) => {
                if (app.id === appId) return false;
                const pos = desktopLayout.value.positions?.[app.id] ?? defaultPos(index);
                return pos.x === target.x && pos.y === target.y;
              });
              if (!occupied) {
                saveIconPosition(appId, target);
              }
            }
          }
        }
        draggingId.value = null;
        dropSessionStarted = false;
      };
      document.addEventListener('pointermove', onMove);
      document.addEventListener('pointerup', onUp);
    };

    const open = (appId: string) => openWindow(appId);

    /** X1 拖拽源域 = 拖拽发生时的当前空间 */
    const sourceSpaceOf = (): SpaceRef =>
      currentSpace.value.type === 'org' ? { type: 'org', orgId: currentSpace.value.orgId } : { type: 'personal' };
    const appNameOf = (appId: string) => appList.value.find((a) => a.id === appId)?.name ?? appId;

    /** X8 复制应用链接已迁入 AppIconMenu（与 Launchpad 共用） */
    /** D15 引导卡片的「发起第一个事务」：开 L12 所有事务顶级对话框（跨域聚合列表） */
    const openAffairs = () => openShellModal('affairs');
    /** D15 引导中的空间名（随当前空间切换；个人空间名可自定义） */
    const spaceName = computed(() =>
      currentSpace.value.type === 'personal' ? personalSpaceName.value : findOrg(currentSpaceOrgId.value)?.name ?? '组织空间'
    );

    // 桌面空白处右键菜单（参照 ark：光标定位 + 边界翻转 + 遮罩关闭）
    const wallpaperInput = ref<HTMLInputElement | null>(null);
    const blankMenu = reactive({ visible: false, x: 0, y: 0 });
    const iconMenu = reactive({ visible: false, x: 0, y: 0, appId: '' });
    const MENU_W = 208;
    /* 菜单高度按条目数估算（每项 36px + 上下内边距 8px）：空白菜单 5 项、图标菜单 5 项，
       用于越界向内翻转（D17），宁可多留不可裁切 */
    const MENU_H = 5 * 36 + 8;
    const ICON_MENU_H = 5 * 36 + 8;
    /** 光标定位 + 视口四边向内翻转（D17：不超出屏幕，也不贴负边） */
    const placeMenu = (x: number, y: number, height: number) => ({
      x: Math.max(8, Math.min(x, window.innerWidth - MENU_W - 8)),
      y: Math.max(8, Math.min(y, window.innerHeight - height - 8))
    });
    const onBlankContext = (e: MouseEvent) => {
      // 空白处右键：清图标选中，菜单弹出在光标处（越界时向内翻）
      selectedId.value = null;
      closeIconMenu();
      Object.assign(blankMenu, placeMenu(e.clientX, e.clientY, MENU_H));
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
      Object.assign(iconMenu, placeMenu(e.clientX, e.clientY, ICON_MENU_H));
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
      } else if (command === 'arrange-grid') {
        setArrangeMode('grid');
      } else if (command === 'arrange-free') {
        setArrangeMode('free');
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

    return {
      selectedId,
      orderedApps,
      loadError,
      refreshAppRegistry,
      desktopLayout,
      arrangeMode,
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
      sourceSpaceOf,
      iconItemOf,
      open,
      openAffairs,
      spaceName,
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
  transition: left var(--spark-dur-fast) var(--spark-ease-standard), top var(--spark-dur-fast) var(--spark-ease-standard);
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
  /* D7：桌面图标投影（令牌，深浅色各自适配），换任意壁纸后仍清晰 */
  box-shadow: var(--spark-desktop-icon-shadow);
}

.pc-icon-name {
  font-size: var(--spark-font-size-secondary);
  /* D7 走查修正：Windows 式固定白色文字 + 深色描边感阴影（令牌），与壁纸/主题解耦 */
  color: var(--spark-desktop-icon-text-color);
  text-shadow: var(--spark-desktop-icon-text-shadow);
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

/* D15 空桌面引导：「这是什么 + 下一步」两张入口卡片 */
.pc-guide {
  max-width: 520px;
  margin: 0 auto;
  padding: var(--spark-padding-page);
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-xl);
  box-shadow: var(--spark-shadow-card);
}

.pc-guide-title {
  font-size: var(--spark-font-size-title);
  font-weight: 600;
  color: var(--spark-text-1);
}

.pc-guide-desc {
  margin: 8px 0 16px;
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-2);
  line-height: 1.6;
}

.pc-guide-actions {
  display: flex;
  gap: var(--spark-gap-page);
}

.pc-guide-card {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 14px;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  text-align: left;
  transition: background var(--spark-dur-fast) var(--spark-ease-standard), border-color var(--spark-dur-fast) var(--spark-ease-standard);
}

.pc-guide-card:hover {
  background: var(--spark-bg-hover);
  border-color: var(--spark-primary);
}

.pc-guide-card-title {
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
}

.pc-guide-card-desc {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  line-height: 1.5;
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
