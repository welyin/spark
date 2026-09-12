<!-- PC 空间桌面宿主（阶段 2 / shell-desktop §三）：进入「空间」后主工作区成为可多窗口的桌面。
     组装：DesktopIconGrid（图标）+ 窗口层（WindowFrame × N，按 zIndex 层叠）+ AppLaunchpad（D4 铺层）+ TaskDock（底部）。
     桌面尺寸经容器测量传入 WindowFrame（D5：移动不设边界，仅作抓回兜底）；切空间时窗口集随 window-manager 换桶。 -->
<template>
  <!-- 桌面背景：默认高级灰（令牌），本空间壁纸（本机存储的本地图片）优先；
       D16：--spark-space-accent 为本空间主题色（自定义或按 spaceId 派生），桌面层统一取用 -->
  <section class="pc-desktop" :style="desktopStyle">
    <div ref="desktopEl" class="desktop-workarea" data-drop-zone="desktop">
    <DesktopIconGrid @open-enable-apps="openSpaceMarket" @open-props="openProps" />
    <!-- D16 域名水印：桌面右下角淡显当前空间名（z 低于窗口层——窗口 zIndex≥1 会盖住它，
         壁纸之上、图标之间；随空间切换） -->
    <div class="pc-desktop-watermark" aria-hidden="true">{{ watermarkText }}</div>

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
    <!-- D4 所有应用 Launchpad 铺层：层级低于 Dock（九宫格可再点关闭） -->
    <AppLaunchpad v-if="launchpadOpen" @close="launchpadOpen = false" @open-enable-apps="openSpaceMarket" @open-props="openProps" />
    <TaskDock :launchpad-open="launchpadOpen" @toggle-launchpad="launchpadOpen = !launchpadOpen" />
    <!-- X1/X2 跨域投递确认 / X4 拖放态跟随提示 / X5 导入预检 -->
    <CrossDomainDropConfirm />
    <DropFeedbackHint />
    <FileImportPreflight />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, reactive, ref, watch, type PropType } from 'vue';
import DesktopIconGrid from './DesktopIconGrid.vue';
import WindowFrame from './WindowFrame.vue';
import TaskDock from './TaskDock.vue';
import AppLaunchpad from './AppLaunchpad.vue';
import CrossDomainDropConfirm from './CrossDomainDropConfirm.vue';
import DropFeedbackHint from './DropFeedbackHint.vue';
import FileImportPreflight from './FileImportPreflight.vue';
import { osDropState } from '../../stores/os-file-drop';
import { activeWinKey, focusWindow, openNewWindow, openWindow, restorePersistedWindows, startWindowsPersistence, windowGroups, windows } from '../../stores/desktop/window-manager';
import { desktopSpaceId, refreshAppRegistry } from '../../stores/desktop/app-registry';
import { desktopLayout } from '../../stores/desktop/layout';
import { currentSpace, currentSpaceOrgId } from '../../stores/current-space';
import { personalSpaceName } from '../../stores/personal-space';
import { findOrg } from '../../stores/org-membership';
import { hashColor } from '../../utils/palette';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';

export default defineComponent({
  name: 'PcDesktop',
  components: { DesktopIconGrid, WindowFrame, TaskDock, AppLaunchpad, CrossDomainDropConfirm, DropFeedbackHint, FileImportPreflight },
  emits: ['open-app'],
  setup(_, { emit }) {
    const desktopEl = ref<HTMLElement | null>(null);
    const bounds = reactive({ width: 1200, height: 800 });
    /** D4 所有应用 Launchpad 铺层开关 */
    const launchpadOpen = ref(false);
    /** 应用属性（图标右键「属性」）：桌面窗口承载（appId='spark:app-properties'，
        目标应用经 viewBootstrap.cardData.appId 传入）；已开着则复用并换目标、置顶 */
    const openProps = (appId: string) => {
      launchpadOpen.value = false;
      const existing = windows.value.find((w) => w.appId === 'spark:app-properties');
      if (existing) {
        existing.viewBootstrap = { cardData: { appId } };
        focusWindow(existing.key);
      } else {
        openNewWindow('spark:app-properties', { viewBootstrap: { cardData: { appId } } });
      }
    };

    /** 空间的应用市场（桌面引导卡 / Launchpad 空态）：空间内桌面窗口（spark:space-market），
        只控制当前空间启停——与系统层应用管理（rail 对话框）完全分开（走查修正） */
    const openSpaceMarket = () => {
      launchpadOpen.value = false;
      openWindow('spark:space-market');
    };

    /** 壁纸（本机 dataURL，按空间）+ D16 每空间主题色（--spark-space-accent 供 Dock/水印取用） */
    const desktopStyle = computed(() => {
      const bg = desktopLayout.value.background;
      const accent = desktopLayout.value.themeColor ?? hashColor(desktopSpaceId.value);
      return {
        '--spark-space-accent': accent,
        ...(bg ? { backgroundImage: `url(${bg})`, backgroundSize: 'cover', backgroundPosition: 'center' } : {})
      } as Record<string, string>;
    });

    /** D16 域名水印文本：当前空间名（个人空间名可自定义 / 组织名） */
    const watermarkText = computed(() =>
      currentSpace.value.type === 'personal'
        ? personalSpaceName.value
        : findOrg(currentSpaceOrgId.value)?.name ?? '组织空间'
    );

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

    // 切空间（组件未销毁时）：恢复目标空间的窗口集；Launchpad 口径随空间变化，顺手收起
    watch(desktopSpaceId, () => {
      launchpadOpen.value = false;
      restorePersistedWindows();
    });

    // X4 拖放态禁止光标：OS 文件拖入且指针不在可承接落点上时，全局 no-drop
    // （壳层对象拖拽的本域重排始终可承接，不挂禁止态）；全局规则见 styles/app-shell.css
    const dropDenied = computed(() => {
      const os = osDropState.value;
      return os.active && (!os.allowed || !os.target);
    });
    watch(dropDenied, (denied) => {
      document.body.classList.toggle('spark-drop-denied', denied);
    });
    onUnmounted(() => document.body.classList.remove('spark-drop-denied'));

    return { desktopEl, bounds, space, desktopStyle, watermarkText, windowGroups, windows, activeWinKey, launchpadOpen, openProps, openSpaceMarket, emit };
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

/* D16 域名水印：桌面右下角淡显当前空间名；pointer-events 关闭不挡操作，
   颜色取本空间主题色（--spark-space-accent），深浅主题用同一低透明度令牌口径 */
.pc-desktop-watermark {
  position: absolute;
  right: 20px;
  bottom: 8px;
  font-size: var(--spark-font-size-title);
  font-weight: 600;
  color: var(--spark-space-accent, var(--spark-text-3));
  opacity: 0.28;
  pointer-events: none;
  user-select: none;
}
</style>
