<!-- D4 所有应用 Launchpad 铺层（problem D4，交互参考 macOS Launchpad）：
     点 Dock 九宫格铺开，展示当前空间可用 / 已启用的全部应用（app-registry 口径，随空间切换而变；
     区别于 L4「应用管理」弹窗：这里是启动器，应用管理是管理界面）。
     毛玻璃背景走 --spark-launchpad-* 令牌；点空白 / 按 Esc / 再点九宫格关闭；
     点图标打开该应用窗口并关闭铺层。 -->
<template>
  <div class="launchpad" @click.self="emit('close')">
    <div v-if="apps.length > 0" class="launchpad-grid">
      <button
        v-for="app in apps"
        :key="app.id"
        type="button"
        class="launchpad-icon"
        :title="app.name"
        @click="open(app.id)"
        @contextmenu.prevent.stop="onIconContext(app.id, $event)"
      >
        <AppIcon class="launchpad-badge" :item="iconItemOf(app)" />
        <span class="launchpad-name">{{ app.name }}</span>
      </button>
    </div>
    <div v-else class="launchpad-empty">
      <!-- 空态入口＝「为本空间启用」（同桌面引导卡口径）；获取代码走 Dock 应用市场（系统层） -->
      <el-empty :image-size="90" description="当前空间还没有已启用的应用">
        <el-button type="primary" @click="emit('open-enable-apps')">为本空间启用应用</el-button>
      </el-empty>
    </div>

    <!-- 图标右键菜单：与桌面图标共用 AppIconMenu（同一套右键效果，走查修正） -->
    <AppIconMenu
      :visible="menu.visible"
      :x="menu.x"
      :y="menu.y"
      :app-id="menu.appId"
      :space="space"
      @close="menu.visible = false"
      @action="emit('close')"
      @open-props="(id) => emit('open-props', id)"
    />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, reactive } from 'vue';
import { appList, getMarketItem } from '../../stores/desktop/app-registry';
import { openWindow } from '../../stores/desktop/window-manager';
import { currentSpace } from '../../stores/current-space';
import AppIconMenu from './AppIconMenu.vue';
import AppIcon from '../apps/AppIcon.vue';
import type { AppIconItem } from '../apps/app-icon';
import type { SpaceRef } from '../../stores/cross-domain-drop';

export default defineComponent({
  name: 'AppLaunchpad',
  components: { AppIconMenu, AppIcon },
  emits: ['close', 'open-enable-apps', 'open-props'],
  setup(_, { emit }) {
    /** 当前空间可用 / 已启用的全部应用（注册表口径，按名称序，随空间切换重建） */
    const apps = appList;

    /** 图标条目：优先市场条目（含声明图标/安装态），无条目退化为最小形状（plugin-dist §2.3） */
    const iconItemOf = (app: { id: string; name: string }): AppIconItem =>
      getMarketItem(app.id) ?? { id: app.id, name: app.name };

    const open = (appId: string) => {
      openWindow(appId);
      emit('close');
    };

    /** 图标右键菜单（与桌面同口径：光标定位 + 越界内翻） */
    const menu = reactive({ visible: false, x: 0, y: 0, appId: '' });
    const onIconContext = (appId: string, e: MouseEvent) => {
      menu.appId = appId;
      menu.x = Math.max(8, Math.min(e.clientX, window.innerWidth - 176));
      menu.y = Math.max(8, Math.min(e.clientY, window.innerHeight - 196));
      menu.visible = true;
    };
    const space = computed<SpaceRef>(() =>
      currentSpace.value.type === 'org'
        ? { type: 'org', orgId: currentSpace.value.orgId }
        : { type: 'personal' }
    );

    const onKeydown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') emit('close');
    };
    onMounted(() => document.addEventListener('keydown', onKeydown));
    onUnmounted(() => document.removeEventListener('keydown', onKeydown));

    return { apps, open, menu, onIconContext, space, emit, iconItemOf };
  }
});
</script>

<style scoped>
.launchpad {
  position: absolute;
  inset: 0;
  /* 层级：压在桌面图标与窗口层之上（.desktop-workarea 为隔离层叠上下文），
     低于 Dock（--spark-z-nav，且 DOM 序在后），九宫格可再点关闭 */
  z-index: var(--spark-z-base);
  background: var(--spark-launchpad-bg);
  backdrop-filter: blur(var(--spark-launchpad-blur)) saturate(1.4);
  -webkit-backdrop-filter: blur(var(--spark-launchpad-blur)) saturate(1.4);
  overflow-y: auto;
  padding: 56px 64px 128px;
}

.launchpad-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, 96px);
  justify-content: center;
  gap: 28px 24px;
  max-width: 880px;
  margin: 0 auto;
}

.launchpad-icon {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  width: 96px;
  padding: 10px 4px;
  border: 0;
  border-radius: var(--spark-radius-l);
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  transition: background var(--spark-dur-fast) var(--spark-ease-standard), transform var(--spark-dur-fast) var(--spark-ease-standard);
}

.launchpad-icon:hover {
  background: var(--spark-bg-hover);
  transform: translateY(-2px);
}

.launchpad-badge {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 56px;
  height: 56px;
  border-radius: var(--spark-radius-l);
  color: var(--spark-text-on-color);
  font-size: 24px;
  font-weight: 600;
  box-shadow: var(--spark-desktop-icon-shadow);
}

.launchpad-name {
  font-size: var(--spark-font-size-secondary);
  /* 与桌面图标同口径（D7 走查修正）：固定白色文字 + 深色描边感阴影 */
  color: var(--spark-desktop-icon-text-color);
  text-shadow: var(--spark-desktop-icon-text-shadow);
  max-width: 100%;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.launchpad-empty {
  padding: 80px 0;
}
</style>
