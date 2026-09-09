<!-- PC 桌面窗口（阶段 2 / ui-architecture §4.2，蓝本 ark-desktop-main §3.5）：
     单个插件窗口壳 = 标题栏（拖动/最大化/最小化/关闭）+ PluginIframeHost 内容 + 八向缩放热区。
     关键算法（ark 已验证，直接用）：
       - iframe 遮罩：非激活 / 拖动缩放中给 iframe 盖透明 mask（iframe 吞鼠标事件的必踩坑）；
       - rect 本地持有（不进全局表），最小化 v-show 保活插件运行态；
       - Pointer Events（兼容触屏/触控板），右/下边界夹取，贴边磁吸（2.4 增强）。
     Spark 不采用 ark 的「iframe 固定宽再 scale」兜底——要求插件响应式，只留最小宽夹取。 -->
<template>
  <div v-if="isVisible && snapPreview !== 'free'" class="window-snap-preview" :style="previewStyle" />
  <div
    v-show="isVisible && !inst.minimized"
    class="window-frame"
    :class="{ active: isActive, dragging: interacting }"
    :style="frameStyle"
    @pointerdown="focusSelf"
  >
    <!-- 标题栏（高 44 复用 topbar）：拖动区 + 控制钮 -->
    <div class="window-titlebar" @pointerdown="onTitlePointerDown" @dblclick="toggleMaximize">
      <span class="window-title-icon" :style="{ background: iconBg }">{{ def?.icon ?? '?' }}</span>
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
      <AppsPage v-if="inst.appId === 'spark:market'" initial-view="market" @open-plugin-tab="emit('open-app', $event)" @changed="refreshAppRegistry" />
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
import { computed, defineComponent, onUnmounted, reactive, ref, watch, type PropType } from 'vue';
import { Close, CopyDocument, FullScreen, Minus, Grid } from '@element-plus/icons-vue';
import PluginIframeHost from '../plugin/PluginIframeHost.vue';
import AppsPage from '../../pages/AppsPage.vue';
import BuiltinAppHost from '../plugin/BuiltinAppHost.vue';
import MessagesPage from '../../pages/MessagesPage.vue';
import AffairsPage from '../../pages/AffairsPage.vue';
import MinePage from '../../pages/MinePage.vue';
import SettingsPage from '../../pages/SettingsPage.vue';
import TestPage from '../../pages/TestPage.vue';
import { setBuiltinImpl } from '../../stores/builtin-apps';
import { refreshCurrentUser } from '../../stores/current-user';
import { findOrg } from '../../stores/org-membership';
import { clampRect, placedRect, snapAt, type Placement, type Rect } from '../../stores/desktop/geometry';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { getApp, refreshAppRegistry, type AppDef } from '../../stores/desktop/app-registry';
import { cascadeIndex, closeWindow, focusWindow, minimizeWindow, openWindow, type WinInst } from '../../stores/desktop/window-manager';
import { hashGradient } from '../../utils/palette';

type ResizeDir = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

const MIN_W = 320;
const MIN_H = 220;
/** 贴边磁吸阈值（px，2.4 半屏/四分屏的触发距离） */
const SNAP = 16;

export default defineComponent({
  name: 'WindowFrame',
  components: { PluginIframeHost, AppsPage, BuiltinAppHost, MessagesPage, AffairsPage, MinePage, SettingsPage, TestPage, Minus, FullScreen, CopyDocument, Close, Grid },
  emits: ['open-app'],
  props: {
    isVisible: { type: Boolean, default: true },
    inst: { type: Object as PropType<WinInst>, required: true },
    isActive: { type: Boolean, default: false },
    space: { type: Object as PropType<PluginSpaceContext>, required: true },
    /** 桌面容器尺寸（边界夹取依据），由桌面层传入 */
    bounds: { type: Object as PropType<{ width: number; height: number }>, required: true }
  },
  setup(props, { emit }) {
    const def = computed<AppDef | null>(() => getApp(props.inst.appId, props.space.type));
    const pluginId = computed(() => def.value?.pluginDomain.slice('plugin:'.length) ?? '');
    const spaceLabel = computed(() => (props.space.type === 'personal' ? '个人空间' : findOrg(props.space.id)?.name ?? '组织空间'));
    const iconBg = computed(() => hashGradient(def.value?.name ?? props.inst.appId));

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
    const minimizeSelf = () => minimizeWindow(props.inst.key);
    const closeSelf = () => closeWindow(props.inst.key);

    const clampToBounds = () => {
      if (props.bounds.width > 0 && props.bounds.height > 0) Object.assign(rect, clampRect(rect, props.bounds));
    };
    watch(() => [props.bounds.width, props.bounds.height], clampToBounds, { immediate: true });
    const setPlacement = (value: Placement) => {
      placement.value = value;
      clampToBounds();
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
          clampToBounds();
          origX = rect.x - (ev.clientX - startX);
          origY = rect.y - (ev.clientY - startY);
        }
        snapPreview.value = snapAt(ev.clientX - area.left, ev.clientY - area.top, props.bounds);
        let nx = origX + (ev.clientX - startX);
        let ny = origY + (ev.clientY - startY);
        // 贴边磁吸（2.4 半屏/四分屏的基础；当前先吸边界）
        if (Math.abs(nx) < SNAP) nx = 0;
        if (Math.abs(ny) < SNAP) ny = 0;
        if (Math.abs(props.bounds.width - (nx + rect.w)) < SNAP) nx = props.bounds.width - rect.w;
        if (Math.abs(props.bounds.height - (ny + rect.h)) < SNAP) ny = props.bounds.height - rect.h;
        rect.x = nx;
        rect.y = ny;
        clampToBounds();
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
        clampToBounds();
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

    return {
      refreshAppRegistry,
      refreshCurrentUser,
      openShellWindow,
      onMessagesFallback,
      emit,
      def,
      pluginId,
      spaceLabel,
      iconBg,
      rect,
      maximized,
      interacting,
      frameStyle,
      snapPreview,
      previewStyle,
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
}

.window-frame.active {
  box-shadow: var(--spark-shadow-pop);
  border-color: var(--spark-border);
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
