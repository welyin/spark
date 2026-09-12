<!-- 手机端空间桌面（space tab 两级结构的第二级，1.2）：
     当前域的「手机系统桌面」——已装插件的应用图标网格，点图标全屏打开插件 App。
     出处 shell-mobile §3.1/§3.2：图标网格/分页/文件夹（分页/文件夹与拖动排序未落地，
     见 docs/ui/problem.md M25/M30 核验注记），
     数据源=当前域已装、本空间可见且已启用的插件（pluginMarket.list 过滤 installed+supportedSpaces
     +per-space 启用（app-enablement），与 PC 桌面注册表同口径），打开走 open-plugin-tab 契约（App.vue 渲染 PluginIframeHost 全屏）。
     图标按空间本机持久化排序（1.4，spark:desktop-icons:<spaceId>），缺省按名称序。
     M30 编辑模式：长按图标进入——图标抖动、点 × 移除快捷方式（仅移出桌面，不卸载应用、
     不删数据，spark:desktop-hidden:<spaceId> 本机持久化）、已移除的可点 + 恢复、
     「添加应用」瓦片进应用 Tab（按当前域作用域）；拖动排序 / 建文件夹未实现。
     M27 最近应用卡片栈（简版）：桌面底部上滑 / 双击 Home 指示条调出——横向卡片列表
     （数据源 useRecentApps，按空间隔离），左右滑浏览、点卡片回到该应用（App.vue 复用
     已存活的插件 tab），点 × 上滑语义＝关闭该应用实例（spark:close-plugin）并移出最近列表。 -->
<template>
  <div
    class="space-desktop"
    @touchstart="onEdgeTouchstart"
    @touchmove="onEdgeTouchmove"
    @touchend="onEdgeTouchend"
    @touchcancel="onEdgeTouchend"
  >
    <!-- 顶部：返回域列表 + 当前域名 -->
    <MobileBackBar :title="spaceName" @back="emit('back')" />

    <!-- M30 编辑模式提示条：口径=移除快捷方式不卸载应用、不删数据 -->
    <div v-if="editing" class="desktop-edit-bar">
      <span class="desktop-edit-hint">编辑桌面：点 × 移除快捷方式（不卸载应用、不删数据）</span>
      <button type="button" class="desktop-edit-done" @click="editing = false">完成</button>
    </div>

    <div class="desktop-body">
      <div v-if="loadError" class="desktop-error">{{ loadError }}</div>

      <!-- 图标网格 -->
      <div v-else class="app-grid" :class="{ editing }">
        <button
          v-for="app in orderedApps"
          :key="app.id"
          type="button"
          class="app-icon"
          @click="onIconClick(app)"
          @touchstart="lp.start($event, app)"
          @touchmove="lp.move"
          @touchend="lp.end"
          @touchcancel="lp.end"
        >
          <AppIcon class="app-icon-badge" :item="app" />
          <span class="app-icon-name">{{ app.name }}</span>
          <!-- M30 编辑模式：移除快捷方式（仅移出桌面，不卸载） -->
          <span
            v-if="editing"
            class="app-icon-remove"
            role="button"
            aria-label="移除快捷方式"
            @click.stop="hideShortcut(app)"
            @touchstart.stop
          >×</span>
        </button>

        <!-- M30 编辑模式：添加应用瓦片（进应用 Tab「为本空间启用」视角，按当前域作用域） -->
        <button v-if="editing" type="button" class="app-icon app-icon-add" @click="emit('open-market')">
          <span class="app-icon-badge app-icon-badge-add">＋</span>
          <span class="app-icon-name">添加应用</span>
        </button>
      </div>

      <!-- M30 编辑模式：已移除的快捷方式（点 + 恢复到桌面） -->
      <template v-if="editing && hiddenApps.length">
        <p class="desktop-hidden-title">已移除的快捷方式</p>
        <div class="app-grid editing">
          <button
            v-for="app in hiddenApps"
            :key="app.id"
            type="button"
            class="app-icon app-icon-hidden"
            @click="restoreShortcut(app)"
          >
            <AppIcon class="app-icon-badge" :item="app" />
            <span class="app-icon-name">{{ app.name }}</span>
            <span class="app-icon-restore" role="button" aria-label="恢复快捷方式">＋</span>
          </button>
        </div>
      </template>

      <!-- 空态：本域无已启用应用时引导去「为本空间启用」（应用 Tab 列表视角，1.6 承接；
           获取代码走应用 Tab 内的市场，系统层） -->
      <div v-if="!loadError && orderedApps.length === 0 && !editing" class="desktop-empty">
        <el-empty :image-size="100" :description="`${spaceName} 还没有已启用的应用`">
          <el-button type="primary" @click="emit('open-market')">为本空间启用应用</el-button>
        </el-empty>
      </div>
    </div>

    <!-- M27 Home 指示条：双击调出最近应用卡片栈（底部上滑手势见根节点触摸处理） -->
    <div class="home-indicator" @click="onIndicatorTap">
      <span class="home-indicator-pill" />
    </div>

    <!-- M27 最近应用卡片栈（简版）：横向卡片，点卡片回到应用，× 关闭实例并移出最近 -->
    <Teleport to="body">
      <Transition name="mobile-sheet">
        <div v-if="recentsVisible" class="recents-root" @click="recentsVisible = false">
          <div class="recents-panel" @click.stop>
            <p class="recents-title">最近使用</p>
            <div v-if="recentApps.length" class="recents-row">
              <div v-for="app in recentApps" :key="app.id" class="recents-card">
                <button type="button" class="recents-card-main" @click="openRecent(app)">
                  <AppIcon class="app-icon-badge" :item="app" />
                  <span class="recents-card-name">{{ app.name }}</span>
                </button>
                <button type="button" class="recents-card-close" aria-label="关闭" @click="closeRecent(app)">×</button>
              </div>
            </div>
            <p v-else class="recents-empty">暂无最近使用的应用</p>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import MobileBackBar from './MobileBackBar.vue';
import AppIcon from './apps/AppIcon.vue';
import { useRecentApps } from './apps/apps-store';
import { isPluginVisibleInSpace } from './apps/space-visibility';
import { currentSpace } from '../stores/current-space';
import { isAppEnabledInSpace } from '../stores/app-enablement';
import { personalSpaceName } from '../stores/personal-space';
import { findOrg } from '../stores/org-membership';
import { createLongPress } from './mobile-long-press';
import { listDevPlugins } from '../mock/dev-plugins';
import type { PluginMarketItemDto } from '../api/types';
import type { OpenPluginTabPayload } from '../pages/AppsPage.vue';

/** M27：底部上滑热区高度（px）与触发位移（px） */
const EDGE_ZONE = 56;
const TRIGGER_DY = 48;
/** Home 指示条双击间隔（ms） */
const DOUBLE_TAP_MS = 350;

export default defineComponent({
  name: 'SpaceDesktop',
  components: { MobileBackBar, AppIcon },
  emits: ['back', 'open-market', 'open-app'],
  setup(_, { emit }) {
    const apps = ref<PluginMarketItemDto[]>([]);
    const loadError = ref('');

    const spaceName = computed(() =>
      currentSpace.value.type === 'personal'
        ? personalSpaceName.value
        : findOrg(currentSpace.value.orgId)?.name ?? '组织空间'
    );

    /** 空间持久化键（1.4）：图标排序按空间本机隔离存储 */
    const spaceId = computed(() =>
      currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    );
    const orderKey = computed(() => `spark:desktop-icons:${spaceId.value}`);
    /** M30：已移除快捷方式（隐藏）列表持久化键 */
    const hiddenKey = computed(() => `spark:desktop-hidden:${spaceId.value}`);

    const loadOrder = (): string[] => {
      try {
        const raw = localStorage.getItem(orderKey.value);
        return raw ? (JSON.parse(raw) as string[]) : [];
      } catch {
        return [];
      }
    };

    /** 已移除（隐藏）的快捷方式 id 列表（响应式，编辑模式即时反映） */
    const hiddenIds = ref<string[]>([]);
    const loadHidden = (): void => {
      try {
        const raw = localStorage.getItem(hiddenKey.value);
        hiddenIds.value = raw ? (JSON.parse(raw) as string[]) : [];
      } catch {
        hiddenIds.value = [];
      }
    };
    const persistHidden = (): void => {
      try {
        localStorage.setItem(hiddenKey.value, JSON.stringify(hiddenIds.value));
      } catch {
        // 持久化失败不阻断本次操作
      }
    };

    /** 当前域可见且本空间已启用的插件（与 PC 桌面注册表同口径：启用读 per-space 事实源
     *  app-enablement，install-and-enable §一②）。启用＝空间层纯逻辑状态，不要求代码在场
     *  （2026-09-10 形式化定义）：已启用未安装的应用照常出现在桌面，打开时由插件宿主
     *  提示就地安装。按空间持久化序 + 名称兜底 */
    const visibleApps = computed(() => {
      const space =
        currentSpace.value.type === 'org'
          ? ({ type: 'org', orgId: currentSpace.value.orgId } as const)
          : ({ type: 'personal' } as const);
      return apps.value.filter(
        (item) =>
          isPluginVisibleInSpace(item.supportedSpaces, currentSpace.value.type) &&
          isAppEnabledInSpace(space, item)
      );
    });
    const orderedApps = computed(() => {
      const shown = visibleApps.value.filter((item) => !hiddenIds.value.includes(item.id));
      const order = loadOrder();
      const indexOf = (id: string) => {
        const idx = order.indexOf(id);
        return idx === -1 ? Number.MAX_SAFE_INTEGER : idx;
      };
      return [...shown].sort((a, b) => {
        const diff = indexOf(a.id) - indexOf(b.id);
        return diff !== 0 ? diff : a.name.localeCompare(b.name, 'zh');
      });
    });
    /** 已移除快捷方式的应用（编辑模式下可恢复） */
    const hiddenApps = computed(() =>
      visibleApps.value.filter((item) => hiddenIds.value.includes(item.id))
    );

    const refresh = async () => {
      try {
        const real = await window.electronAPI.pluginMarket.list();
        // 按 id 去重：内核条目优先于 dev 注入条目（同插件不重复出现）
        const merged = new Map<string, PluginMarketItemDto>();
        for (const item of listDevPlugins()) {
          merged.set(item.id, item);
        }
        for (const item of real) {
          merged.set(item.id, item);
        }
        apps.value = [...merged.values()];
        loadError.value = '';
      } catch (err) {
        loadError.value = `加载应用失败：${err}`;
      }
    };

    onMounted(() => {
      loadHidden();
      void refresh();
    });
    // 切域后重载（桌面数据随空间隔离；隐藏列表同步重载、退出编辑态）
    watch(() => currentSpace.value, () => {
      editing.value = false;
      loadHidden();
      void refresh();
    });

    /** 打开应用：按 open-plugin-tab 契约上报（App.vue 渲染 PluginIframeHost 全屏） */
    const openApp = (app: PluginMarketItemDto) => {
      emit('open-app', {
        pluginDomain: app.domain,
        pluginView: app.views[0] ?? 'default',
        title: app.name,
        icon: app.name.slice(0, 1),
        pluginContext: currentSpace.value.type === 'org' ? { orgId: currentSpace.value.orgId } : undefined
      } satisfies OpenPluginTabPayload);
    };

    // ------------------------------------------------------------------
    // M30 长按编辑模式：抖动 + 移除/恢复快捷方式 + 添加应用瓦片
    // ------------------------------------------------------------------
    const editing = ref(false);
    const lp = createLongPress<PluginMarketItemDto>(() => {
      editing.value = true;
    });

    /** 编辑模式下点图标不打开应用（防误触），正常态打开 */
    const onIconClick = (app: PluginMarketItemDto) => {
      if (editing.value) {
        return;
      }
      openApp(app);
    };

    const hideShortcut = (app: PluginMarketItemDto) => {
      hiddenIds.value = [...hiddenIds.value, app.id];
      persistHidden();
      ElMessage.success(`已移除「${app.name}」快捷方式（应用未卸载，数据保留）`);
    };
    const restoreShortcut = (app: PluginMarketItemDto) => {
      hiddenIds.value = hiddenIds.value.filter((id) => id !== app.id);
      persistHidden();
    };

    // ------------------------------------------------------------------
    // M27 最近应用卡片栈（简版）：底部上滑 / 双击 Home 指示条调出
    // ------------------------------------------------------------------
    const recent = useRecentApps(spaceId);
    const recentsVisible = ref(false);
    const recentApps = computed(() =>
      recent.recentIds.value
        .map((id) => visibleApps.value.find((item) => item.id === id))
        .filter((item): item is PluginMarketItemDto => Boolean(item))
    );

    /** 点卡片回到该应用（App.vue 的 openPluginTab 对已存活的 tab 直接切换，不重建实例） */
    const openRecent = (app: PluginMarketItemDto) => {
      recentsVisible.value = false;
      openApp(app);
    };

    /** × = 上滑关闭语义：关实例（spark:close-plugin，App.vue 监听）+ 移出最近列表 */
    const closeRecent = (app: PluginMarketItemDto) => {
      window.dispatchEvent(
        new CustomEvent('spark:close-plugin', { detail: { pluginDomain: app.domain } })
      );
      recent.removeRecent(app.id);
    };

    // Home 指示条双击
    let lastIndicatorTap = 0;
    const onIndicatorTap = () => {
      const now = Date.now();
      if (now - lastIndicatorTap < DOUBLE_TAP_MS) {
        recentsVisible.value = true;
        lastIndicatorTap = 0;
      } else {
        lastIndicatorTap = now;
      }
    };

    // 底部上滑手势（编辑模式下不接管）
    let edgeStartY: number | null = null;
    let edgeDy = 0;
    const onEdgeTouchstart = (event: TouchEvent) => {
      if (editing.value || event.touches.length !== 1) {
        return;
      }
      const touch = event.touches[0];
      if (window.innerHeight - touch.clientY > EDGE_ZONE) {
        return;
      }
      edgeStartY = touch.clientY;
      edgeDy = 0;
    };
    const onEdgeTouchmove = (event: TouchEvent) => {
      if (edgeStartY === null) {
        return;
      }
      const touch = event.touches[0];
      if (touch) {
        edgeDy = touch.clientY - edgeStartY;
      }
    };
    const onEdgeTouchend = () => {
      if (edgeStartY === null) {
        return;
      }
      edgeStartY = null;
      if (edgeDy <= -TRIGGER_DY) {
        recentsVisible.value = true;
      }
    };

    return {
      spaceName,
      orderedApps,
      hiddenApps,
      loadError,
      onIconClick,
      editing,
      lp,
      hideShortcut,
      restoreShortcut,
      recentsVisible,
      recentApps,
      openRecent,
      closeRecent,
      onIndicatorTap,
      onEdgeTouchstart,
      onEdgeTouchmove,
      onEdgeTouchend,
      emit
    };
  }
});
</script>

<style scoped>
.space-desktop {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page);
}

.desktop-body {
  flex: 1;
  overflow-y: auto;
  padding: var(--spark-padding-page);
}

.desktop-error {
  padding: 20px;
  text-align: center;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.app-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(var(--spark-desktop-icon-size), 1fr));
  gap: var(--spark-desktop-icon-gap);
  justify-items: center;
}

.app-icon {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  width: var(--spark-desktop-icon-size);
  padding: 6px 2px;
  border: 0;
  border-radius: var(--spark-radius-l);
  background: transparent;
  cursor: pointer;
  font-family: inherit;
}

.app-icon:hover {
  background: var(--spark-bg-hover);
}

.app-icon-badge {
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

.app-icon-name {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-1);
  max-width: 100%;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.desktop-empty {
  padding: 40px 0;
}

/* ---- M30 编辑模式 ---- */
.desktop-edit-bar {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px var(--spark-padding-page);
  background: var(--spark-bg-card);
  border-bottom: 1px solid var(--spark-border-light);
}

.desktop-edit-hint {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.desktop-edit-done {
  flex-shrink: 0;
  border: 0;
  background: transparent;
  color: var(--spark-primary);
  font-family: inherit;
  font-size: 15px;
  cursor: pointer;
}

/* 抖动动画（微信式，两档相位错开） */
@keyframes desktop-jiggle-a {
  0%, 100% { transform: rotate(-1.5deg); }
  50% { transform: rotate(1.5deg); }
}

@keyframes desktop-jiggle-b {
  0%, 100% { transform: rotate(1.5deg); }
  50% { transform: rotate(-1.5deg); }
}

.app-grid.editing .app-icon {
  animation: desktop-jiggle-a 0.25s ease-in-out infinite;
}

.app-grid.editing .app-icon:nth-child(2n) {
  animation-name: desktop-jiggle-b;
}

.app-icon-remove,
.app-icon-restore {
  position: absolute;
  top: 0;
  right: 8px;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  color: var(--spark-text-2);
  font-size: 14px;
  line-height: 1;
  box-shadow: var(--spark-shadow-card);
  cursor: pointer;
}

.app-icon-add .app-icon-badge-add {
  background: transparent;
  border: 1px dashed var(--spark-border-light);
  color: var(--spark-text-3);
  box-shadow: none;
}

.app-icon-hidden {
  opacity: 0.55;
}

.desktop-hidden-title {
  margin: 16px 0 8px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

/* ---- M27 Home 指示条 + 最近应用卡片栈 ---- */
.home-indicator {
  flex-shrink: 0;
  display: flex;
  justify-content: center;
  padding: 6px 0 calc(6px + var(--spark-safe-bottom, env(safe-area-inset-bottom, 0px)));
}

.home-indicator-pill {
  width: 120px;
  height: 4px;
  border-radius: 2px;
  background: var(--spark-text-3);
  opacity: 0.4;
}

.recents-root {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
  background: rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: flex-end;
}

.recents-panel {
  width: 100%;
  background: var(--spark-bg-card);
  border-radius: var(--spark-radius-xl) var(--spark-radius-xl) 0 0;
  padding: 12px 0 calc(12px + var(--spark-safe-bottom, env(safe-area-inset-bottom, 0px)));
}

.recents-title {
  margin: 0 0 8px;
  padding: 0 var(--spark-padding-page);
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.recents-row {
  display: flex;
  gap: 12px;
  overflow-x: auto;
  padding: 4px var(--spark-padding-page);
}

.recents-card {
  position: relative;
  flex-shrink: 0;
  width: 96px;
}

.recents-card-main {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 12px 4px;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  background: var(--spark-bg-page);
  cursor: pointer;
  font-family: inherit;
}

.recents-card-name {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-1);
  max-width: 100%;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.recents-card-close {
  position: absolute;
  top: -6px;
  right: -6px;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 1px solid var(--spark-border-light);
  background: var(--spark-bg-card);
  color: var(--spark-text-2);
  font-size: 14px;
  line-height: 1;
  cursor: pointer;
  box-shadow: var(--spark-shadow-card);
}

.recents-empty {
  margin: 0;
  padding: 16px var(--spark-padding-page);
  text-align: center;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

/* 上滑面板进出过渡（与 SpaceListPage 上滑菜单同名动画，mobile-sheet 全局类） */
.mobile-sheet-enter-active,
.mobile-sheet-leave-active {
  transition: opacity var(--spark-dur-page) var(--spark-ease-ios);
}

.mobile-sheet-enter-from,
.mobile-sheet-leave-to {
  opacity: 0;
}
</style>
