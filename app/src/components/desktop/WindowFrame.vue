<!-- PC 桌面窗口（阶段 2 / ui-architecture §4.2，蓝本 ark-desktop-main §3.5）：
     单个插件窗口壳 = 标题栏（拖动/最大化/最小化/关闭）+ PluginIframeHost 内容 + 八向缩放热区。
     关键算法（ark 已验证，直接用）：
       - iframe 遮罩：非激活 / 拖动缩放中给 iframe 盖透明 mask（iframe 吞鼠标事件的必踩坑）；
       - rect 本地持有（不进全局表），最小化 v-show 保活插件运行态；
       - Pointer Events（兼容触屏/触控板），移动不设边界（D5：仅兜底标题栏留一小段可抓回），贴边磁吸（2.4 增强）。
     Spark 不采用 ark 的「iframe 固定宽再 scale」兜底——要求插件响应式，只留最小宽夹取。 -->
<template>
  <div v-if="isVisible && snapPreview !== 'free'" class="window-snap-preview" :style="previewStyle" />
  <div
    ref="frameEl"
    v-show="isVisible && !inst.minimized"
    class="window-frame"
    :class="{ active: isActive, dragging: interacting, leaving, entering }"
    :style="[frameStyle, originStyle]"
    @pointerdown="focusSelf"
  >
    <!-- 标题栏（高 44 复用 topbar）：拖动区 + 控制钮 -->
    <div class="window-titlebar" @pointerdown="onTitlePointerDown" @dblclick="toggleMaximize">
      <AppIcon class="window-title-icon" :item="iconItem" />
      <span class="window-title">{{ def?.name ?? inst.appId }}</span>
      <span class="window-space" :title="spaceLabel">{{ spaceLabel }}</span>
      <div class="window-controls">
        <el-dropdown trigger="click" @command="setPlacement">
          <button type="button" class="window-ctl" title="窗口布局" @pointerdown.stop @dblclick.stop>
            <el-icon :size="14"><Grid /></el-icon>
          </button>
          <template #dropdown>
            <el-dropdown-menu>
              <el-dropdown-item command="free">还原</el-dropdown-item>
              <el-dropdown-item command="left">左半屏</el-dropdown-item>
              <el-dropdown-item command="right">右半屏</el-dropdown-item>
              <el-dropdown-item command="top-left">左上四分屏</el-dropdown-item>
              <el-dropdown-item command="top-right">右上四分屏</el-dropdown-item>
              <el-dropdown-item command="bottom-left">左下四分屏</el-dropdown-item>
              <el-dropdown-item command="bottom-right">右下四分屏</el-dropdown-item>
            </el-dropdown-menu>
          </template>
        </el-dropdown>
        <button type="button" class="window-ctl" title="最小化" @pointerdown.stop @click.stop="minimizeSelf">
          <el-icon :size="14"><Minus /></el-icon>
        </button>
        <button type="button" class="window-ctl" :title="maximized ? '还原' : '最大化'" @pointerdown.stop @click.stop="toggleMaximize">
          <el-icon :size="14"><component :is="maximized ? CopyDocument : FullScreen" /></el-icon>
        </button>
        <button type="button" class="window-ctl window-ctl-close" title="关闭" @pointerdown.stop @click.stop="closeSelf">
          <el-icon :size="14"><Close /></el-icon>
        </button>
      </div>
    </div>

    <!-- 内容：内置壳层页面（spark:*）或插件 iframe（遮罩在非激活/交互中盖上，防吞事件） -->
    <div class="window-body">
      <!-- 应用市场窗口（A34 灰度）：旧内置 UI ⇄ 默认内置插件版（spark-market，与 apps tab 同一注册项）；
           initial-view="market" 直达市场页（与 legacy 同口径，插件版经 viewId 透传） -->
      <BuiltinAppHost v-if="inst.appId === 'spark:market'" tab-id="apps" :space="space" initial-view="market" @fallback="onAppsFallback">
        <template #legacy>
          <AppsPage initial-view="market" @open-plugin-tab="emit('open-app', $event)" @changed="refreshAppRegistry" />
        </template>
      </BuiltinAppHost>
      <BuiltinAppHost v-else-if="inst.appId === 'spark:messages'" tab-id="messages" :space="space" @fallback="onMessagesFallback">
        <template #legacy><MessagesPage /></template>
      </BuiltinAppHost>
      <AffairsPage v-else-if="inst.appId === 'spark:affairs'" />
      <MinePage v-else-if="inst.appId === 'spark:mine'" @profile-updated="refreshCurrentUser" />
      <SettingsPage
        v-else-if="inst.appId === 'spark:settings'"
        @profile-updated="refreshCurrentUser"
        @open-tab="openShellWindow"
        @back-root="closeSelf"
      />
      <TestPage v-else-if="inst.appId === 'spark:test'" @back-root="closeSelf" />
      <!-- 应用属性窗口（图标右键「属性」，桌面窗口而非对话框）：目标应用经 viewBootstrap.cardData.appId 传入 -->
      <AppPropertiesPanel v-else-if="inst.appId === 'spark:app-properties'" :app-id="propsTargetAppId" />
      <!-- 空间的应用市场（桌面窗口，与系统层应用管理完全分开）：目录＋详情内启用/停用 -->
      <SpaceMarketView v-else-if="inst.appId === 'spark:space-market'" />
      <PluginIframeHost
        v-else-if="def"
        :key="inst.key"
        :plugin-id="pluginId"
        :view-id="inst.viewId ?? def.view"
        :view-bootstrap="inst.viewBootstrap"
        :space="space"
        @close="closeSelf"
      />
      <div v-else class="window-missing">
        <el-empty :image-size="80" description="应用不可用（可能已卸载）" />
      </div>
      <!-- iframe 遮罩：非激活窗 + 拖动/缩放中盖上透明层，把鼠标事件留给外壳 -->
      <div v-if="!isActive || interacting" class="iframe-mask" @pointerdown="focusSelf" />
    </div>

    <!-- 八向缩放热区（四边 + 四角） -->
    <template v-if="!maximized">
      <div
        v-for="dir in resizeDirs"
        :key="dir"
        class="window-resize"
        :class="`window-resize-${dir}`"
        @pointerdown="onResizePointerDown(dir, $event)"
      />
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, reactive, ref, watch, type PropType } from 'vue';
import { Close, CopyDocument, FullScreen, Minus, Grid } from '@element-plus/icons-vue';
import PluginIframeHost from '../plugin/PluginIframeHost.vue';
import AppsPage from '../../pages/AppsPage.vue';
import BuiltinAppHost from '../plugin/BuiltinAppHost.vue';
import MessagesPage from '../../pages/MessagesPage.vue';
import AffairsPage from '../../pages/AffairsPage.vue';
import MinePage from '../../pages/MinePage.vue';
import SettingsPage from '../../pages/SettingsPage.vue';
import TestPage from '../../pages/TestPage.vue';
import AppPropertiesPanel from './AppPropertiesPanel.vue';
import SpaceMarketView from '../apps/SpaceMarketView.vue';
import { setBuiltinImpl } from '../../stores/builtin-apps';
import { refreshCurrentUser } from '../../stores/current-user';
import { findOrg } from '../../stores/org-membership';
import { personalSpaceName } from '../../stores/personal-space';
import { keepTitlebarGrabbable, placedRect, snapAt, type Placement, type Rect } from '../../stores/desktop/geometry';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { getApp, getMarketItem, refreshAppRegistry, type AppDef } from '../../stores/desktop/app-registry';
import { cascadeIndex, closeWindow, focusWindow, minimizeWindow, openWindow, type WinInst } from '../../stores/desktop/window-manager';
import AppIcon from '../apps/AppIcon.vue';
import type { AppIconItem } from '../apps/app-icon';

type ResizeDir = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

const MIN_W = 320;
const MIN_H = 220;
/** 贴边磁吸阈值（px，2.4 半屏/四分屏的触发距离） */
const SNAP = 16;

export default defineComponent({
  name: 'WindowFrame',
  components: { PluginIframeHost, AppsPage, BuiltinAppHost, MessagesPage, AffairsPage, MinePage, SettingsPage, TestPage, AppPropertiesPanel, SpaceMarketView, Minus, FullScreen, CopyDocument, Close, Grid, AppIcon },
  emits: ['open-app'],
  props: {
    isVisible: { type: Boolean, default: true },
    inst: { type: Object as PropType<WinInst>, required: true },
    isActive: { type: Boolean, default: false },
    space: { type: Object as PropType<PluginSpaceContext>, required: true },
    /** 桌面容器尺寸（抓回兜底 / 贴边磁吸依据），由桌面层传入 */
    bounds: { type: Object as PropType<{ width: number; height: number }>, required: true }
  },
  setup(props, { emit }) {
    const def = computed<AppDef | null>(() => getApp(props.inst.appId, props.space.type));
    const pluginId = computed(() => def.value?.pluginDomain.slice('plugin:'.length) ?? '');
    const spaceLabel = computed(() => (props.space.type === 'personal' ? personalSpaceName.value : findOrg(props.space.id)?.name ?? '组织空间'));
    /** 属性窗口的目标应用 id（viewBootstrap.cardData.appId；缺失回退空串→面板显示读取失败） */
    const propsTargetAppId = computed(() => {
      const cardData = props.inst.viewBootstrap?.cardData;
      if (cardData && typeof cardData === 'object' && typeof (cardData as { appId?: unknown }).appId === 'string') {
        return (cardData as { appId: string }).appId;
      }
      return '';
    });
    /** 标题栏图标条目：优先市场条目（含声明图标/安装态），壳层内置窗口（spark:*）
        无市场条目退化为最小形状走首字符回退（plugin-dist §2.3 回退链） */
    const iconItem = computed<AppIconItem>(() =>
      getMarketItem(props.inst.appId) ?? { id: props.inst.appId, name: def.value?.name ?? props.inst.appId }
    );
    // rect 本地持有（参照 ark：不进全局表）
    const initW = () => Math.min(def.value?.defaultWidth ?? 880, Math.max(MIN_W, props.bounds.width - 48));
    const initH = () => Math.min(def.value?.defaultHeight ?? 620, Math.max(MIN_H, props.bounds.height - 48));
    const rect = reactive({
      x: 40 + cascadeIndex(props.inst.key) * 20,
      y: 24 + cascadeIndex(props.inst.key) * 20,
      w: initW(),
      h: initH()
    });
    const placement = ref<Placement>('free');
    const maximized = computed(() => placement.value === 'maximized');
    const snapPreview = ref<Placement>('free');
    const interacting = ref(false);
    const geometryKey = `spark:window-geometry:${props.space.id}:${props.inst.appId}`;
    try {
      const stored = JSON.parse(localStorage.getItem(geometryKey) ?? 'null');
      if (stored && ['x', 'y', 'w', 'h'].every((key) => Number.isFinite(stored.rect?.[key]))) {
        Object.assign(rect, stored.rect);
        if (props.inst.seq > 1) { rect.x += 20 * (props.inst.seq - 1); rect.y += 20 * (props.inst.seq - 1); }
        if (['free', 'maximized', 'left', 'right', 'top-left', 'top-right', 'bottom-left', 'bottom-right'].includes(stored.placement)) {
          placement.value = stored.placement;
        }
      }
    } catch {}
    const styleOf = (value: Rect) => ({ left: `${value.x}px`, top: `${value.y}px`, width: `${value.w}px`, height: `${value.h}px`, zIndex: props.inst.zIndex });
    const frameStyle = computed(() => styleOf(placement.value === 'free' ? rect : placedRect(placement.value, props.bounds)));
    const previewStyle = computed(() => snapPreview.value === 'free' ? {} : styleOf(placedRect(snapPreview.value, props.bounds)));
    const saveGeometry = () => {
      try { localStorage.setItem(geometryKey, JSON.stringify({ rect: { ...rect }, placement: placement.value })); } catch {}
    };
    let endInteraction: (() => void) | null = null;

    const focusSelf = () => focusWindow(props.inst.key);
    const closeSelf = () => animateOut(() => closeWindow(props.inst.key));

    /* ---- W6 开/收窗 0.15s scale 动效，锚定 Dock 图标位置（transform-origin 指向 Dock 图标中心） ----
       动效时长与 --spark-dur-fast（150ms）一致；离场动画播完再真正最小化/关窗（v-show/移除）。 */
    const ANIM_MS = 150;
    const frameEl = ref<HTMLElement | null>(null);
    const leaving = ref(false);
    /** W6 入场动效改为一次性 class（entering）：只在挂载与最小化还原时播放——
        之前动画挂在 .window-frame 基类上，拖动时 .dragging 置 animation:none、
        松手移除该类导致 pop-in 重播（移动后窗口"重新从小变大"的走查缺陷） */
    const entering = ref(false);
    const playEnter = () => {
      entering.value = true;
      setTimeout(() => {
        entering.value = false;
      }, ANIM_MS);
    };
    const originStyle = ref<Record<string, string>>({});
    onMounted(() => {
      playEnter();
      const frame = frameEl.value;
      const dockIcon = document.querySelector(`[data-dock-app="${props.inst.appId}"]`);
      if (!frame || !dockIcon) {
        return;
      }
      const fr = frame.getBoundingClientRect();
      const dr = dockIcon.getBoundingClientRect();
      if (fr.width <= 0 || fr.height <= 0) {
        return;
      }
      const ox = (((dr.left + dr.width / 2 - fr.left) / fr.width) * 100).toFixed(1);
      const oy = (((dr.top + dr.height / 2 - fr.top) / fr.height) * 100).toFixed(1);
      originStyle.value = { '--window-dock-origin': `${ox}% ${oy}%` };
    });
    // 最小化 → 还原（v-show 恢复）时重播入场动效（锚点仍指向 Dock 图标）
    watch(
      () => props.inst.minimized,
      (minimized, was) => {
        if (was && !minimized) {
          playEnter();
        }
      }
    );
    function animateOut(action: () => void) {
      if (leaving.value) {
        return;
      }
      leaving.value = true;
      setTimeout(() => {
        leaving.value = false;
        action();
      }, ANIM_MS);
    }
    const minimizeSelf = () => animateOut(() => minimizeWindow(props.inst.key));

    /** D5 窗口移动不设边界：不再夹取进桌面，仅兜底「标题栏至少留一小段在屏内可抓回」。
        工作区尺寸变化 / 切换布局 / 缩放后同样只套这层兜底，不把窗口强行拉回屏内。 */
    const keepGrabbable = () => {
      if (props.bounds.width > 0 && props.bounds.height > 0) Object.assign(rect, keepTitlebarGrabbable(rect, props.bounds));
    };
    watch(() => [props.bounds.width, props.bounds.height], keepGrabbable, { immediate: true });
    const setPlacement = (value: Placement) => {
      placement.value = value;
      keepGrabbable();
      focusSelf();
      saveGeometry();
    };
    const toggleMaximize = () => {
      setPlacement(maximized.value ? 'free' : 'maximized');
    };

    /** 拖动（标题栏）：Pointer Events，记起点 + document move/up */
    const onTitlePointerDown = (e: PointerEvent) => {
      if (e.button !== 0 || (e.target as HTMLElement).closest('.window-controls, .window-network')) {
        return;
      }
      const area = (e.currentTarget as HTMLElement).parentElement?.parentElement?.getBoundingClientRect();
      if (!area) return;
      e.preventDefault();
      focusSelf();
      interacting.value = true;
      const startX = e.clientX;
      const startY = e.clientY;
      let origX = rect.x;
      let origY = rect.y;
      const onMove = (ev: PointerEvent) => {
        if (Math.abs(ev.clientX - startX) + Math.abs(ev.clientY - startY) < 4) return;
        if (placement.value !== 'free') {
          placement.value = 'free';
          rect.x = ev.clientX - area.left - rect.w / 2;
          rect.y = ev.clientY - area.top - 22;
          keepGrabbable();
          origX = rect.x - (ev.clientX - startX);
          origY = rect.y - (ev.clientY - startY);
        }
        snapPreview.value = snapAt(ev.clientX - area.left, ev.clientY - area.top, props.bounds);
        let nx = origX + (ev.clientX - startX);
        let ny = origY + (ev.clientY - startY);
        // 贴边磁吸（2.4 半屏/四分屏的基础；当前先吸边界）。D5：拖动不设边界，
        // 磁吸与自由拖出不冲突（只在靠近边 16px 内生效），保留。
        if (Math.abs(nx) < SNAP) nx = 0;
        if (Math.abs(ny) < SNAP) ny = 0;
        if (Math.abs(props.bounds.width - (nx + rect.w)) < SNAP) nx = props.bounds.width - rect.w;
        if (Math.abs(props.bounds.height - (ny + rect.h)) < SNAP) ny = props.bounds.height - rect.h;
        rect.x = nx;
        rect.y = ny;
        keepGrabbable();
      };
      const onUp = () => {
        if (snapPreview.value !== 'free') setPlacement(snapPreview.value);
        finish();
      };
      const finish = () => {
        interacting.value = false;
        snapPreview.value = 'free';
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        document.removeEventListener('pointercancel', finish);
        endInteraction = null;
        saveGeometry();
      };
      endInteraction = finish;
      document.addEventListener('pointermove', onMove);
      document.addEventListener('pointerup', onUp);
      document.addEventListener('pointercancel', finish);
    };

    /** 八向缩放：四边+四角热区，向左/上缩放同步改 x/y，带最小宽夹取 */
    const onResizePointerDown = (dir: ResizeDir, e: PointerEvent) => {
      if (e.button !== 0) return;
      if (placement.value !== 'free') {
        Object.assign(rect, placedRect(placement.value, props.bounds));
        placement.value = 'free';
      }
      e.preventDefault();
      e.stopPropagation();
      focusSelf();
      interacting.value = true;
      const startX = e.clientX;
      const startY = e.clientY;
      const orig = { ...rect };
      const onMove = (ev: PointerEvent) => {
        const dx = ev.clientX - startX;
        const dy = ev.clientY - startY;
        if (dir.includes('e')) rect.w = orig.w + dx;
        if (dir.includes('s')) rect.h = orig.h + dy;
        if (dir.includes('w')) {
          rect.w = orig.w - dx;
          rect.x = orig.x + dx;
        }
        if (dir.includes('n')) {
          rect.h = orig.h - dy;
          rect.y = orig.y + dy;
        }
        // 最小宽夹取（保持对侧边不动）
        if (rect.w < MIN_W) {
          if (dir.includes('w')) rect.x = orig.x + (orig.w - MIN_W);
          rect.w = MIN_W;
        }
        if (rect.h < MIN_H) {
          if (dir.includes('n')) rect.y = orig.y + (orig.h - MIN_H);
          rect.h = MIN_H;
        }
        keepGrabbable();
      };
      const onUp = () => {
        interacting.value = false;
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        document.removeEventListener('pointercancel', onUp);
        endInteraction = null;
        saveGeometry();
      };
      endInteraction = onUp;
      document.addEventListener('pointermove', onMove);
      document.addEventListener('pointerup', onUp);
      document.addEventListener('pointercancel', onUp);
    };

    onUnmounted(() => {
      endInteraction?.();
      saveGeometry();
      interacting.value = false;
    });

    // 内置壳层窗口事件：设置页 open-tab → 开对应壳窗口；消息插件版失败 → 回退 legacy
    const openShellWindow = (tab: string) => {
      openWindow(`spark:${tab}`);
    };
    const onMessagesFallback = () => setBuiltinImpl('messages', 'legacy');
    // 市场插件版加载失败/被关闭：回退该 tab 的旧内置 UI（与 messages 同口径灰度兜底）
    const onAppsFallback = () => setBuiltinImpl('apps', 'legacy');

    return {
      refreshAppRegistry,
      refreshCurrentUser,
      openShellWindow,
      onMessagesFallback,
      onAppsFallback,
      emit,
      def,
      pluginId,
      spaceLabel,
      iconItem,
      rect,
      maximized,
      interacting,
      frameStyle,
      snapPreview,
      previewStyle,
      frameEl,
      leaving,
      entering,
      propsTargetAppId,
      originStyle,
      setPlacement,
      resizeDirs: ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'] as ResizeDir[],
      focusSelf,
      minimizeSelf,
      closeSelf,
      toggleMaximize,
      onTitlePointerDown,
      onResizePointerDown,
      Minus,
      FullScreen,
      CopyDocument,
      Close
    };
  }
});
</script>

<style scoped>
.window-frame {
  position: absolute;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  box-shadow: var(--spark-shadow-hover);
  overflow: hidden;
  transition: box-shadow var(--spark-dur-fast) var(--spark-ease-standard);
  /* W6：开/收窗 0.15s scale 动效，锚点 = Dock 图标中心（--window-dock-origin 由脚本按 data-dock-app 实测注入，缺省底边中点）；
     入场动效只在 .entering（挂载 / 最小化还原）时播放，拖动 / 缩放不再触发重播 */
  transform-origin: var(--window-dock-origin, 50% 100%);
}

.window-frame.entering {
  animation: window-pop-in var(--spark-dur-fast) var(--spark-ease-standard);
}

.window-frame.leaving {
  animation: window-pop-out var(--spark-dur-fast) var(--spark-ease-standard) forwards;
}

@keyframes window-pop-in {
  from { transform: scale(0.85); opacity: 0; }
  to { transform: scale(1); opacity: 1; }
}

@keyframes window-pop-out {
  from { transform: scale(1); opacity: 1; }
  to { transform: scale(0.85); opacity: 0; }
}

.window-frame.active {
  box-shadow: var(--spark-shadow-pop);
  border-color: var(--spark-border);
}

/* 非激活窗口（Windows/macOS 式退后感）：投影减弱、标题栏文字与控件降灰，
   与激活窗口一眼可辨；内容区不动（不遮罩、不降透明度，保持可读可操作） */
.window-frame:not(.active) {
  box-shadow: var(--spark-shadow-card);
}

.window-frame:not(.active) .window-titlebar {
  color: var(--spark-text-3);
}

.window-frame:not(.active) .window-title,
.window-frame:not(.active) .window-space {
  color: var(--spark-text-3);
}

.window-frame:not(.active) .window-title-icon {
  filter: saturate(0.6);
  opacity: 0.85;
}

.window-frame.dragging {
  transition: none;
  user-select: none;
}

.window-titlebar {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  height: var(--spark-window-titlebar-height);
  padding: 0 8px 0 12px;
  background: var(--spark-bg-card);
  border-bottom: 1px solid var(--spark-border-light);
  cursor: move;
  touch-action: none;
}

.window-title-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: var(--spark-radius-s);
  color: var(--spark-text-on-color);
  font-size: 12px;
  font-weight: 600;
  flex-shrink: 0;
}

.window-title {
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.window-space {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  min-width: 0;
  max-width: 100px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.window-snap-preview {
  position: absolute;
  pointer-events: none;
  background: var(--spark-primary-light);
  border: 2px solid var(--spark-primary);
  border-radius: var(--spark-radius-l);
  opacity: 0.6;
}

.window-controls {
  margin-left: auto;
  display: flex;
  gap: 2px;
  flex-shrink: 0;
}

.window-ctl {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: 0;
  border-radius: var(--spark-radius-m);
  background: transparent;
  color: var(--spark-text-2);
  cursor: pointer;
}

.window-ctl:hover {
  background: var(--spark-bg-hover);
  color: var(--spark-text-1);
}

.window-ctl-close:hover {
  background: var(--spark-danger);
  color: var(--spark-text-on-color);
}

.window-body {
  flex: 1;
  min-height: 0;
  position: relative;
}

.window-body :deep(.plugin-iframe-host) {
  height: 100%;
}

/* 窗口形态最小高 220：去掉 PluginIframeHost 面向整页场景的 480px 最小高兜底，
   否则小窗下 iframe 被外壳裁切、插件底部输入区不可达。类名重复一次提升优先级，
   确定性压过组件 scoped 样式（同优先级时依赖源码序，不稳定） */
.window-body :deep(.plugin-iframe-host.plugin-iframe-host) {
  min-height: 0;
}

.window-body :deep(.plugin-iframe-frame.plugin-iframe-frame) {
  min-height: 0;
}

.window-missing {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}

/* iframe 遮罩（ark 必踩坑）：非激活/交互中盖上，把指针事件留给外壳 */
.iframe-mask {
  position: absolute;
  inset: 0;
  z-index: 1;
  background: transparent;
  cursor: default;
}

/* 八向缩放热区 */
.window-resize {
  position: absolute;
  z-index: 2;
  touch-action: none;
}

.window-resize-n { top: -3px; left: 8px; right: 8px; height: 6px; cursor: ns-resize; }
.window-resize-s { bottom: -3px; left: 8px; right: 8px; height: 6px; cursor: ns-resize; }
.window-resize-e { right: -3px; top: 8px; bottom: 8px; width: 6px; cursor: ew-resize; }
.window-resize-w { left: -3px; top: 8px; bottom: 8px; width: 6px; cursor: ew-resize; }
.window-resize-ne { top: -3px; right: -3px; width: 12px; height: 12px; cursor: nesw-resize; }
.window-resize-nw { top: -3px; left: -3px; width: 12px; height: 12px; cursor: nwse-resize; }
.window-resize-se { bottom: -3px; right: -3px; width: 12px; height: 12px; cursor: nwse-resize; }
.window-resize-sw { bottom: -3px; left: -3px; width: 12px; height: 12px; cursor: nesw-resize; }
</style>
