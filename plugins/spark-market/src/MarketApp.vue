<!-- 应用市场插件根组件（A34，移植自壳层 AppsPage 的系统层语义）：
     清单（已装应用管理）⇄ 市场（收录/探索/开发者）⇄ 详情 三视图切换；
     数据面全部走 sdk.market（桥），动作（安装/更新/卸载/仓库安装/侧载）
     经 src/app-actions.ts（权限确认在内）。
     插件版边界（见任务报告遗留）：mock/dev 注入条目、空间层启停
     （per-space app-enablement）、壳层系统通知回写不迁移——灰度回退
     legacy（壳层 stores/builtin-apps）可恢复旧内置界面。 -->
<template>
  <section class="apps-page market-plugin-root">
    <el-alert v-if="loadError" class="apps-load-error" :title="loadError" type="error" :closable="false" show-icon />

    <AppDetailPanel
      v-if="selectedItem"
      class="market-plugin-detail"
      :item="selectedItem"
      :busy="busyByPlugin[selectedItem.id] ?? ''"
      @back="selectedId = null"
      @install="installApp"
      @upgrade="upgradeApp"
      @uninstall="uninstallApp"
    />

    <AppMarketPanel
      v-else-if="view === 'market'"
      :items="items"
      @back="view = 'list'"
      @detail="openDetail"
      @install="installApp"
      @install-repo="installRepoPlugin"
    />

    <AppListPanel
      v-else
      :installed-items="installedItems"
      :busy-by-plugin="busyByPlugin"
      @detail="openDetail"
      @add-app="view = 'market'"
      @install-repo="installRepoPlugin"
      @sideloaded="refreshSafe"
    />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketItem, PluginMarketRepoDeclaration } from '../../../packages/plugin-sdk/src';
import { marketApi, pluginContext } from './sdk-host';
import { installPluginItem, installRepoDeclaration } from './app-actions';
import { marketInitialView } from './market-store';
import AppDetailPanel from './components/AppDetailPanel.vue';
import AppListPanel from './components/AppListPanel.vue';
import AppMarketPanel from './components/AppMarketPanel.vue';

type ViewName = 'list' | 'market';
type BusyAction = '' | 'install' | 'upgrade' | 'uninstall';

export default defineComponent({
  name: 'MarketApp',
  components: { AppDetailPanel, AppListPanel, AppMarketPanel },
  setup() {
    const items = ref<PluginMarketItem[]>([]);
    const loadError = ref('');
    // 起步子视图（A34 initial-view 直达）：ctx.viewId==='market' 直达市场页
    // （Dock「应用市场」/ spark:market 桌面窗口入口），缺省清单页
    const view = ref<ViewName>(marketInitialView(pluginContext()?.viewId));
    const selectedId = ref<string | null>(null);
    const busyByPlugin = ref<Record<string, BusyAction>>({});

    /** 已安装清单＝系统层全量（L4/M4：应用管理不按空间过滤） */
    const installedItems = computed(() => items.value.filter((item) => item.installed));
    const selectedItem = computed(() => items.value.find((item) => item.id === selectedId.value) ?? null);

    const refresh = async () => {
      const market = marketApi();
      if (!market) {
        return;
      }
      items.value = await market.list();
    };

    const refreshSafe = async () => {
      try {
        await refresh();
        loadError.value = '';
      } catch (error) {
        loadError.value = `加载应用市场失败：${error}`;
      }
    };

    const setBusy = (pluginId: string, action: BusyAction) => {
      busyByPlugin.value = { ...busyByPlugin.value, [pluginId]: action };
    };

    const openDetail = (item: PluginMarketItem) => {
      selectedId.value = item.id;
    };

    const installApp = async (item: PluginMarketItem) => {
      setBusy(item.id, 'install');
      try {
        if (await installPluginItem(item)) {
          await refresh();
        }
      } finally {
        setBusy(item.id, '');
      }
    };

    const installRepoPlugin = async (declaration: PluginMarketRepoDeclaration) => {
      setBusy(declaration.id, 'install');
      try {
        if (await installRepoDeclaration(declaration)) {
          await refresh();
        }
      } finally {
        setBusy(declaration.id, '');
      }
    };

    const upgradeApp = async (item: PluginMarketItem) => {
      const market = marketApi();
      if (!market) {
        return;
      }
      setBusy(item.id, 'upgrade');
      try {
        await market.upgrade(item.id);
        await refresh();
        ElMessage.success('应用已更新到最新版本');
      } catch (error) {
        ElMessage.error(`应用更新失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    // 卸载：系统层本机动作；确认框明示「仅移除插件程序，数据保留在本机」
    const uninstallApp = async (item: PluginMarketItem) => {
      const market = marketApi();
      if (!market) {
        return;
      }
      try {
        await ElMessageBox.confirm(
          '卸载仅移除插件程序，插件数据（文档/消息）保留在本机。已打开的该应用页面将被关闭。',
          `卸载 ${item.name}`,
          { confirmButtonText: '卸载', cancelButtonText: '取消', type: 'warning' }
        );
      } catch {
        return; // 用户取消
      }
      setBusy(item.id, 'uninstall');
      try {
        await market.uninstall(item.id);
        if (selectedId.value === item.id) {
          selectedId.value = null;
        }
        await refreshSafe();
        ElMessage.success('应用已卸载');
      } catch (error) {
        ElMessage.error(`应用卸载失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    onMounted(async () => {
      // 先展示清单（list 是本地目录+安装态聚合，快）；更新探测后台异步执行
      // （逐插件串行拉远端 manifest+验签，网络差时慢），完成后 refresh 反映
      // 「可更新」角标。失败静默（同壳层语义，不阻断页面使用）。
      await refreshSafe();
      marketApi()
        ?.checkUpdates()
        .then(() => refreshSafe())
        .catch(() => {});
    });

    return {
      items,
      loadError,
      view,
      selectedId,
      selectedItem,
      busyByPlugin,
      installedItems,
      openDetail,
      installApp,
      installRepoPlugin,
      upgradeApp,
      uninstallApp,
      refreshSafe
    };
  }
});
</script>

<style>
/* 市场插件全局样式（移植壳层 styles/pages/apps.css + apps-market.css；
   不加 scoped——卡片样式跨子组件类名共享，与壳层 AppsPage 同口径） */
@import './styles/apps.css';
@import './styles/apps-market.css';

.market-plugin-root {
  height: 100%;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
}

/* 详情整页（插件内无壳层抽屉/移动栈，详情为整页切换 + 顶部返回） */
.market-plugin-detail {
  height: 100%;
  display: flex;
  flex-direction: column;
}
</style>
