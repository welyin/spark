<!-- PC 空间桌面宿主（阶段 2 / shell-desktop §三）：进入「空间」后主工作区成为可多窗口的桌面。
     组装：DesktopIconGrid（图标）+ 窗口层（WindowFrame × N，按 zIndex 层叠）+ TaskDock（底部）。
     桌面尺寸经容器测量传入 WindowFrame 做边界夹取；切空间时窗口集随 window-manager 换桶。 -->
<template>
  <!-- 桌面背景：默认高级灰（令牌），本空间壁纸（本机存储的本地图片）优先 -->
  <section class="pc-desktop" :style="desktopStyle">
    <div ref="desktopEl" class="desktop-workarea">
    <DesktopIconGrid @open-market="emit('open-market')" />

    <!-- 窗口层：只渲染当前空间非删除的实例（最小化经 v-show 保活） -->
    <template v-for="group in windowGroups" :key="group.space.id">
    <WindowFrame
      v-for="w in group.windows"
      :key="w.key"
      :is-visible="group.space.id === space.id"
      :inst="w"
      :is-active="activeWinKey === w.key"
      :space="group.space"
      :bounds="bounds"
      @open-app="emit('open-app', $event)"
    />
    </template>

    </div>
    <TaskDock @open-market="emit('open-market')" />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, reactive, ref, watch, type PropType } from 'vue';
import DesktopIconGrid from './DesktopIconGrid.vue';
import WindowFrame from './WindowFrame.vue';
import TaskDock from './TaskDock.vue';
import { activeWinKey, restorePersistedWindows, startWindowsPersistence, windowGroups, windows } from '../../stores/desktop/window-manager';
import { desktopSpaceId, refreshAppRegistry } from '../../stores/desktop/app-registry';
import { desktopLayout } from '../../stores/desktop/layout';
import { currentSpace } from '../../stores/current-space';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';

export default defineComponent({
  name: 'PcDesktop',
  components: { DesktopIconGrid, WindowFrame, TaskDock },
  emits: ['open-market', 'open-app'],
  setup(_, { emit }) {
    const desktopEl = ref<HTMLElement | null>(null);
    const bounds = reactive({ width: 1200, height: 800 });

    /** 壁纸（本机 dataURL，按空间）；缺省 = 样式表中的高级灰令牌背景 */
    const desktopStyle = computed(() => {
      const bg = desktopLayout.value.background;
      return bg ? { backgroundImage: `url(${bg})`, backgroundSize: 'cover', backgroundPosition: 'center' } : {};
    });

    const space = computed<PluginSpaceContext>(() => ({
      type: currentSpace.value.type,
      id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    }));

    let observer: ResizeObserver | null = null;
    onMounted(() => {
      // 兜底刷新注册表（electronAPI 就绪后拿到已装清单；模块级首拉可能因时机过早为空）
      void refreshAppRegistry();
      // 窗口持久化：启动变化监听（幂等）+ 恢复当前空间上次开着的应用（单实例）
      startWindowsPersistence();
      restorePersistedWindows();
      if (desktopEl.value && typeof ResizeObserver !== 'undefined') {
        if (desktopEl.value.clientWidth > 0 && desktopEl.value.clientHeight > 0) {
          bounds.width = desktopEl.value.clientWidth;
          bounds.height = desktopEl.value.clientHeight;
        }
        observer = new ResizeObserver((entries) => {
          const entry = entries[0];
          if (entry && entry.contentRect.width > 0 && entry.contentRect.height > 0) {
            bounds.width = entry.contentRect.width;
            bounds.height = entry.contentRect.height;
          }
        });
        observer.observe(desktopEl.value);
      }
    });
    onUnmounted(() => {
      observer?.disconnect();
    });

    // 切空间（组件未销毁时）：恢复目标空间的窗口集
    watch(desktopSpaceId, () => restorePersistedWindows());

    return { desktopEl, bounds, space, desktopStyle, windowGroups, windows, activeWinKey, emit };
  }
});
</script>

<style scoped>
.pc-desktop {
  position: relative;
  height: 100%;
  overflow: hidden;
  /* 默认桌面背景：高级灰（令牌，深浅主题随 --el-* 别名自动变） */
  background: var(--spark-bg-hover);
  isolation: isolate;
}
.desktop-workarea { position: absolute; inset: 0 0 92px; isolation: isolate; }
</style>
