<!-- 应用图标右键菜单（桌面图标与 Launchpad 图标共用，走查修正：两处右键效果一致）：
     打开 / 新窗口打开 / 固定到任务栏 / 复制应用链接 / 属性。
     「属性」打开独立的属性窗口（AppPropertiesDialog）；装卸不在此（系统层应用管理）。
     光标定位 Teleport 弹层、越界向内翻转、点遮罩/右键关闭（D17 口径）。 -->
<template>
  <Teleport to="body">
    <div
      v-if="visible"
      class="app-icon-menu-mask"
      @click="emit('close')"
      @contextmenu.prevent="emit('close')"
    >
      <div class="app-icon-menu" :style="{ left: `${x}px`, top: `${y}px` }" @click.stop>
        <button type="button" class="app-icon-menu-item" @click="onCommand('open')">打开</button>
        <button type="button" class="app-icon-menu-item" @click="onCommand('new')">新窗口打开</button>
        <button type="button" class="app-icon-menu-item" @click="onCommand('pin')">
          {{ pinned ? '取消固定到任务栏' : '固定到任务栏' }}
        </button>
        <button type="button" class="app-icon-menu-item" @click="onCommand('copy-link')">复制应用链接</button>
        <button type="button" class="app-icon-menu-item" @click="onCommand('props')">属性</button>
      </div>
    </div>
  </Teleport>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { openNewWindow, openWindow } from '../../stores/desktop/window-manager';
import { desktopLayout, togglePinned } from '../../stores/desktop/layout';
import { buildSparkUrl } from '../../services/deep-link';
import type { SpaceRef } from '../../stores/cross-domain-drop';

export default defineComponent({
  name: 'AppIconMenu',
  props: {
    visible: { type: Boolean, required: true },
    x: { type: Number, required: true },
    y: { type: Number, required: true },
    appId: { type: String, required: true },
    /** 当前空间（复制链接的域寻址用） */
    space: { type: Object as PropType<SpaceRef>, required: true }
  },
  emits: ['close', 'action', 'open-props'],
  setup(props, { emit }) {
    const pinned = computed(() => desktopLayout.value.dock.includes(props.appId));

    const copyLink = async () => {
      // X8/X9：只含寻址字段，不含密钥数据
      const url = buildSparkUrl(props.space, props.appId);
      try {
        await navigator.clipboard.writeText(url);
        ElMessage.success('已复制应用链接');
      } catch {
        ElMessage.warning(`复制失败，可手动复制：${url}`);
      }
    };

    const onCommand = (command: string) => {
      emit('close');
      if (command === 'open') openWindow(props.appId);
      else if (command === 'new') openNewWindow(props.appId);
      else if (command === 'pin') togglePinned(props.appId);
      else if (command === 'copy-link') void copyLink();
      else if (command === 'props') emit('open-props', props.appId);
      // 任何动作都让宿主知道（Launchpad 借此收起铺层）
      emit('action');
    };

    return { pinned, onCommand, emit };
  }
});
</script>

<style scoped>
.app-icon-menu-mask {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
}

.app-icon-menu {
  position: fixed;
  min-width: 160px;
  padding: 4px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  box-shadow: var(--spark-shadow-pop);
  display: flex;
  flex-direction: column;
}

.app-icon-menu-item {
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

.app-icon-menu-item:hover {
  background: var(--spark-bg-hover);
}
</style>
