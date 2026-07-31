<template>
  <section class="apps-page">
    <el-alert
      v-if="loadError"
      class="apps-load-error"
      :title="loadError"
      type="error"
      :closable="false"
      show-icon
    />

    <AppListPanel
      v-if="view === 'list'"
      :installed-items="installedItems"
      :recent-items="recentItems"
      :is-enabled="isEnabled"
      :is-suspended="isSuspended"
      :groups="groups"
      @open="openApp"
      @detail="(item) => openDetail(item)"
      @toggle="toggleEnabled"
      @add-app="view = 'market'"
    />

    <AppMarketPanel
      v-else-if="view === 'market'"
      :items="items"
      @back="view = 'list'"
      @detail="(item) => openDetail(item)"
      @install="installApp"
      @install-repo="installRepoPlugin"
      @sideloaded="refreshSafe"
    />

    <!-- 应用详情：全 app 统一抽屉（无头部小标题，右上角自定义关闭），不再整页切换；
         详情面板内的「返回」按钮同样映射为关闭抽屉 -->
    <el-drawer v-model="detailVisible" :with-header="false" size="520" class="app-drawer">
      <button type="button" class="app-drawer-close" title="关闭" @click="detailVisible = false">
        <el-icon :size="16"><Close /></el-icon>
      </button>
      <div class="app-drawer-body">
        <AppDetailPanel
          v-if="selectedItem"
          :item="selectedItem"
          :enabled="isEnabled(selectedItem)"
          :is-org-space="isOrgSpace"
          :is-admin="isCurrentUserAdmin"
          :busy="busyByPlugin[selectedItem.id] ?? ''"
          @back="detailVisible = false"
          @open="openApp"
          @install="installApp"
          @upgrade="upgradeApp"
          @toggle="toggleEnabled"
          @request-enable="requestEnable"
        />
      </div>
    </el-drawer>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Close } from '@element-plus/icons-vue';
import type { PluginMarketItemDto, RepoPluginDeclarationDto } from '../api/types';
import { currentSpace } from '../stores/current-space';
import { enablePluginInstance, isPluginInstanceDisabled, pluginInstanceKey } from '../plugin-disabled';
import { isAdmin, refreshOrganizations } from '../stores/org-membership';
import { consumePendingAppDetail, pendingAppDetail } from '../stores/pending-app';
import { isMockApp, listMockApps, setMockAppEnabled, setMockAppInstalled } from '../mock/apps';
import { mockMode } from '../mock/mode';
import { spaceKeyOf } from '../mock/space-key';
import { notifyPluginInstalled, notifyPluginUpgraded } from '../app-messages';
import AppListPanel from '../components/apps/AppListPanel.vue';
import AppMarketPanel from '../components/apps/AppMarketPanel.vue';
import AppDetailPanel from '../components/apps/AppDetailPanel.vue';
import {
  permissionLabel,
  useAppGroups,
  useOrgEnabled,
  useRecentApps
} from '../components/apps/apps-store';

export type OpenPluginTabPayload = {
  pluginDomain: string;
  pluginView: string;
  title: string;
  icon: string;
  pluginContext?: {
    orgId?: string;
  };
};

type ViewName = 'list' | 'market';
type BusyAction = '' | 'install' | 'upgrade' | 'toggle';

export default defineComponent({
  name: 'AppsPage',
  components: { AppListPanel, AppMarketPanel, AppDetailPanel, Close },
  emits: ['open-plugin-tab'],
  setup(_, { emit }) {
    const items = ref<PluginMarketItemDto[]>([]);
    const realItems = ref<PluginMarketItemDto[]>([]);
    const loadError = ref('');
    const view = ref<ViewName>('list');
    /** 应用详情抽屉（点击卡片不再整页切换） */
    const detailVisible = ref(false);
    const selectedId = ref<string | null>(null);
    const busyByPlugin = ref<Record<string, BusyAction>>({});

    // 空间上下文：个人空间=安装/启用/禁用；组织空间=只暴露启用/禁用（ui-apps-market §4.2）
    const isOrgSpace = computed(() => currentSpace.value.type === 'org');
    const spaceKey = computed(() =>
      currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    );

    const groups = useAppGroups(spaceKey);
    const recent = useRecentApps(spaceKey);
    const orgEnabled = useOrgEnabled(spaceKey);

    // 是否当前组织管理员：org-membership 共享缓存的成员角色判断（§4.2/§5.2）
    const isCurrentUserAdmin = computed(() =>
      currentSpace.value.type === 'org' ? isAdmin(currentSpace.value.orgId) : false
    );
    const refreshAdminRole = async () => {
      if (currentSpace.value.type !== 'org') {
        return;
      }
      try {
        await refreshOrganizations();
      } catch {
        // 读取失败保留旧缓存；无缓存时按非管理员展示
      }
    };

    const isEnabled = (item: PluginMarketItemDto): boolean =>
      isOrgSpace.value ? orgEnabled.isOrgEnabled(item.id) : item.enabled;

    /** 熔断实例键：卡片置灰与打开拦截同一口径（plugin-disabled.ts） */
    const instanceKeyOf = (item: PluginMarketItemDto) =>
      pluginInstanceKey(item.id, {
        type: currentSpace.value.type,
        id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
      });

    /** 崩溃环自动停用（卡片级置灰 + 「已停用」徽标）；localStorage 读取，页面重进时刷新 */
    const isSuspended = (item: PluginMarketItemDto): boolean => isPluginInstanceDisabled(instanceKeyOf(item));

    const installedItems = computed(() => items.value.filter((item) => item.installed));

    const recentItems = computed(() =>
      recent.recentIds.value
        .map((id) => installedItems.value.find((item) => item.id === id))
        .filter((item): item is PluginMarketItemDto => Boolean(item))
    );

    const selectedItem = computed(
      () => items.value.find((item) => item.id === selectedId.value) ?? null
    );

    // 仅 mock 模式（npm run tauri:mock）把 mock 应用（src/mock/apps.ts）合并进真实市场结果
    const mergeItems = () => {
      items.value = mockMode() ? [...realItems.value, ...listMockApps()] : [...realItems.value];
    };

    const refresh = async () => {
      realItems.value = await window.electronAPI.pluginMarket.list();
      mergeItems();
    };

    const refreshSafe = async () => {
      try {
        await refresh();
        loadError.value = '';
      } catch (error) {
        // 真实市场不可用时仍展示已有条目（mock 模式下含 mock 应用），保证 UI 可看
        mergeItems();
        loadError.value = `加载应用市场失败：${error}`;
      }
    };

    const setBusy = (pluginId: string, action: BusyAction) => {
      busyByPlugin.value = { ...busyByPlugin.value, [pluginId]: action };
    };

    const openDetail = (item: PluginMarketItemDto) => {
      selectedId.value = item.id;
      detailVisible.value = true;
    };

    // 消费「打开应用详情」请求（全局搜索跳转）：市场条目加载完成后找到该应用进入详情
    const openPendingAppDetail = () => {
      const id = pendingAppDetail.value;
      if (!id) {
        return;
      }
      const item = items.value.find((entry) => entry.id === id);
      if (!item) {
        return;
      }
      consumePendingAppDetail();
      openDetail(item);
    };
    watch([pendingAppDetail, items], openPendingAppDetail);

    const openApp = async (item: PluginMarketItemDto) => {
      if (!isEnabled(item)) {
        openDetail(item);
        return;
      }
      // mock 应用无真实插件可加载，打开以 toast 占位（真实插件链路不受影响）
      if (isMockApp(item)) {
        recent.recordOpen(item.id);
        ElMessage.info(`「${item.name}」为演示应用，暂无真实插件视图`);
        return;
      }
      // 熔断自动停用（崩溃环）：入口拦截 + 提示，确认后手动重新启用（清零计数）
      const instanceKey = instanceKeyOf(item);
      if (isPluginInstanceDisabled(instanceKey)) {
        try {
          await ElMessageBox.confirm(
            `「${item.name}」在当前空间因多次异常已自动停用。重新启用将清零异常计数。`,
            '应用已自动停用',
            { confirmButtonText: '重新启用', cancelButtonText: '取消', type: 'warning' }
          );
        } catch {
          return; // 用户取消，保持停用
        }
        enablePluginInstance(instanceKey);
      }
      recent.recordOpen(item.id);
      emit('open-plugin-tab', {
        pluginDomain: item.domain,
        pluginView: item.views[0] ?? 'default',
        title: item.name,
        icon: item.name.slice(0, 1),
        pluginContext: currentSpace.value.type === 'org' ? { orgId: currentSpace.value.orgId } : undefined
      } satisfies OpenPluginTabPayload);
    };

    const installApp = async (item: PluginMarketItemDto) => {
      // 安装前明确展示所需权限（设计 §6.1），授权后才调真实安装接口
      if (item.permissions.length > 0) {
        const labels = item.permissions
          .map((permission) => `${permissionLabel(permission)}（${permission}）`)
          .join('、');
        try {
          await ElMessageBox.confirm(
            `该应用声明以下权限：${labels}。安装即视为授权，运行时可越权调用将被系统拦截。`,
            `授权安装 ${item.name}`,
            { confirmButtonText: '授权并安装', cancelButtonText: '取消', type: 'warning' }
          );
        } catch {
          return; // 用户取消授权
        }
      }
      setBusy(item.id, 'install');
      // mock 应用：权限确认流程保留，安装只写 localStorage 状态（见 src/mock/apps.ts）；
      // 系统通知走同一入口（纯浏览器下由内存镜像入账，便于演示应用会话链路）
      if (isMockApp(item)) {
        setMockAppInstalled(item.id, true);
        mergeItems();
        setBusy(item.id, '');
        ElMessage.success('应用安装成功，启用后即可使用');
        notifyPluginInstalled(spaceKeyOf(currentSpace.value), item.name);
        return;
      }
      try {
        await window.electronAPI.pluginMarket.install(item.id);
        await refresh();
        ElMessage.success('应用安装成功，启用后即可使用');
        // 系统通知样板（app:system 内置应用会话，按当前空间隔离）
        notifyPluginInstalled(spaceKeyOf(currentSpace.value), item.name);
      } catch (error) {
        ElMessage.error(`应用安装失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    // 仓库锚定安装（plugin-dist）：声明文件已在前置解析中展示，此处做权限确认后安装
    const installRepoPlugin = async (declaration: RepoPluginDeclarationDto) => {
      if (declaration.permissions.length > 0) {
        const labels = declaration.permissions
          .map((permission) => `${permissionLabel(permission)}（${permission}）`)
          .join('、');
        try {
          await ElMessageBox.confirm(
            `该应用声明以下权限：${labels}。安装即视为授权，运行时可越权调用将被系统拦截。`,
            `授权安装 ${declaration.name}`,
            { confirmButtonText: '授权并安装', cancelButtonText: '取消', type: 'warning' }
          );
        } catch {
          return; // 用户取消授权
        }
      }
      setBusy(declaration.id, 'install');
      try {
        await window.electronAPI.pluginMarket.installFromRepo(declaration.id);
        await refresh();
        ElMessage.success('应用安装成功，启用后即可使用');
        notifyPluginInstalled(spaceKeyOf(currentSpace.value), declaration.name);
      } catch (error) {
        // 网络差降级（plugin_system.md「市场展示与排序」）：仓库不可达时提示手动侧载路径；
        // 判定走结构化前缀（plugin-dist §6 错误串统一 "Repo plugin ... fetch failed" 形态）
        const message = `${error}`;
        const unreachable =
          message.startsWith('Repo plugin') && message.includes('fetch failed');
        ElMessage.error(
          unreachable
            ? `应用安装失败：仓库不可达，可自行下载 .spkg 后用「导入 .spkg 文件」侧载安装（${message}）`
            : `应用安装失败：${message}`
        );
      } finally {
        setBusy(declaration.id, '');
      }
    };

    const upgradeApp = async (item: PluginMarketItemDto) => {
      if (isMockApp(item)) {
        ElMessage.info('演示应用已是最新版本');
        return;
      }
      setBusy(item.id, 'upgrade');
      try {
        await window.electronAPI.pluginMarket.upgrade(item.id);
        await refresh();
        ElMessage.success('应用已更新到最新版本');
        // 系统通知样板（同安装口径，app:system 内置应用会话）
        notifyPluginUpgraded(spaceKeyOf(currentSpace.value), item.name);
      } catch (error) {
        ElMessage.error(`应用更新失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    const toggleEnabled = async (item: PluginMarketItemDto) => {
      if (isOrgSpace.value) {
        // 组织空间：仅管理员可操作，启用状态为本地 mock（见 apps-store.ts TODO(mock)；mock 应用同走此路径）
        if (!isCurrentUserAdmin.value) {
          await requestEnable(item);
          return;
        }
        orgEnabled.setOrgEnabled(item.id, !orgEnabled.isOrgEnabled(item.id));
        return;
      }
      // mock 应用：启停只写 localStorage 状态
      if (isMockApp(item)) {
        setMockAppEnabled(item.id, !item.enabled);
        mergeItems();
        return;
      }
      setBusy(item.id, 'toggle');
      try {
        await window.electronAPI.pluginMarket.setEnabled(item.id, !item.enabled);
        await refresh();
      } catch (error) {
        ElMessage.error(`应用启停失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    // 非管理员请求启用流程（ui-apps-market §5.2）
    const requestEnable = async (item: PluginMarketItemDto) => {
      try {
        await ElMessageBox.confirm(
          `只有组织管理员可以启用应用。你可以联系管理员请求启用“${item.name}”。`,
          '请求启用应用',
          { confirmButtonText: '联系管理员', cancelButtonText: '取消', type: 'info' }
        );
      } catch {
        return; // 用户取消
      }
      // TODO(mock): 消息模块并行开发中，先以 toast 占位；后续应打开与组织管理员的 1:1 聊天并自动发送应用链接卡片（设计 §5.2 第 4-5 步）
      ElMessage.info('消息功能开发中，暂时无法联系管理员');
    };

    // 切换空间时回到列表主视图并刷新管理员角色
    watch(spaceKey, () => {
      view.value = 'list';
      selectedId.value = null;
      void refreshAdminRole();
    });

    onMounted(async () => {
      void refreshAdminRole();
      await refreshSafe();
      openPendingAppDetail();
      // 本地定期检测更新（设计 §4.4）：进入应用页时检测一次，发现新版本显示「可更新」角标
      try {
        await window.electronAPI.pluginMarket.checkUpdates();
        await refresh();
      } catch {
        // 更新检测失败不阻断页面使用
      }
    });

    return {
      items,
      loadError,
      view,
      detailVisible,
      selectedItem,
      busyByPlugin,
      isOrgSpace,
      isCurrentUserAdmin,
      groups,
      installedItems,
      recentItems,
      isEnabled,
      isSuspended,
      openApp,
      openDetail,
      installApp,
      installRepoPlugin,
      upgradeApp,
      toggleEnabled,
      requestEnable,
      refreshSafe
    };
  }
});
</script>

<!-- 注意：不能加 scoped —— 列表/市场/详情卡片都在子组件（AppListPanel/AppMarketPanel/AppDetailPanel）内渲染，
     scoped 样式只会作用于本组件模板元素，无法穿透子组件，导致卡片样式整体失效（与 MessagesPage 等页面同样用非 scoped） -->
<style src="../styles/pages/apps.css"></style>
<style src="../styles/pages/apps-market.css"></style>
