<!-- 手机端空间桌面（space tab 两级结构的第二级，1.2）：
     当前域的「手机系统桌面」——已装插件的应用图标网格，点图标全屏打开插件 App。
     出处 shell-mobile §3.1/§3.2：图标网格/分页/文件夹（编辑模式 1.3+ 补），
     数据源=当前域已装且本空间可见的插件（pluginMarket.list 过滤 installed+supportedSpaces，
     与 AppsPage 同口径），打开走 open-plugin-tab 契约（App.vue 渲染 PluginIframeHost 全屏）。
     图标按空间本机持久化排序（1.4，spark:desktop-icons:<spaceId>），缺省按名称序。 -->
<template>
  <div class="space-desktop">
    <!-- 顶部：返回域列表 + 当前域名 -->
    <MobileBackBar :title="spaceName" @back="emit('back')" />

    <div class="desktop-body">
      <div v-if="loadError" class="desktop-error">{{ loadError }}</div>

      <!-- 图标网格 -->
      <div v-else class="app-grid">
        <button
          v-for="app in orderedApps"
          :key="app.id"
          type="button"
          class="app-icon"
          @click="openApp(app)"
        >
          <span class="app-icon-badge" :style="{ background: appIconBackground(app) }">{{ iconText(app) }}</span>
          <span class="app-icon-name">{{ app.name }}</span>
        </button>
      </div>

      <!-- 空态：本域未装应用时引导去市场（1.6 承接） -->
      <div v-if="!loadError && orderedApps.length === 0" class="desktop-empty">
        <el-empty :image-size="100" :description="`${spaceName} 还没有应用`">
          <el-button type="primary" @click="emit('open-market')">去应用市场</el-button>
        </el-empty>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import MobileBackBar from './MobileBackBar.vue';
import { appIconBackground } from './apps/apps-store';
import { isPluginVisibleInSpace } from './apps/space-visibility';
import { currentSpace } from '../stores/current-space';
import { findOrg } from '../stores/org-membership';
import type { PluginMarketItemDto } from '../api/types';
import type { OpenPluginTabPayload } from '../pages/AppsPage.vue';

export default defineComponent({
  name: 'SpaceDesktop',
  components: { MobileBackBar },
  emits: ['back', 'open-market', 'open-app'],
  setup(_, { emit }) {
    const apps = ref<PluginMarketItemDto[]>([]);
    const loadError = ref('');

    const spaceName = computed(() =>
      currentSpace.value.type === 'personal'
        ? '个人空间'
        : findOrg(currentSpace.value.orgId)?.name ?? '组织空间'
    );

    /** 空间持久化键（1.4）：图标排序按空间本机隔离存储 */
    const orderKey = computed(() =>
      `spark:desktop-icons:${currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'}`
    );

    const loadOrder = (): string[] => {
      try {
        const raw = localStorage.getItem(orderKey.value);
        return raw ? (JSON.parse(raw) as string[]) : [];
      } catch {
        return [];
      }
    };

    /** 当前域已装且本空间可见的插件（与 AppsPage 同口径），按空间持久化序 + 名称兜底 */
    const orderedApps = computed(() => {
      const visible = apps.value.filter(
        (item) => item.installed && isPluginVisibleInSpace(item.supportedSpaces, currentSpace.value.type)
      );
      const order = loadOrder();
      const indexOf = (id: string) => {
        const idx = order.indexOf(id);
        return idx === -1 ? Number.MAX_SAFE_INTEGER : idx;
      };
      return [...visible].sort((a, b) => {
        const diff = indexOf(a.id) - indexOf(b.id);
        return diff !== 0 ? diff : a.name.localeCompare(b.name, 'zh');
      });
    });

    const iconText = (app: PluginMarketItemDto) => app.name.slice(0, 1);

    const refresh = async () => {
      try {
        apps.value = await window.electronAPI.pluginMarket.list();
        loadError.value = '';
      } catch (err) {
        loadError.value = `加载应用失败：${err}`;
      }
    };

    onMounted(refresh);
    // 切域后重载（桌面数据随空间隔离）
    watch(() => currentSpace.value, refresh);

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

    return { spaceName, orderedApps, loadError, iconText, appIconBackground, openApp, emit };
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
</style>
