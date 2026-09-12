<template>
  <section class="apps-page" :class="{ 'apps-page-mobile-stack': isMobileLayout && (detailVisible || view === 'market' || view === 'enable') }">
    <el-alert
      v-if="loadError"
      class="apps-load-error"
      :title="loadError"
      type="error"
      :closable="false"
      show-icon
    />

    <!-- 列表/市场：桌面端常驻；移动端（波次 2/3）包进导航栈转场层——
         列表为栈1，市场/详情为整页栈帧（MobileBackBar 返回栏），栈帧切换经 MobilePageTransition 滑动转场（微信式） -->
    <MobilePageTransition v-if="isMobileLayout" :tab="MOBILE_TAB">
      <!-- 应用详情：栈顶整页（与桌面抽屉同一份 AppDetailPanel），底部 tab bar 常驻；
           详情可从列表或市场进入，故优先于市场分支判断；
           插件页（栈3）走 App.vue 插件 tab 整页（自带 ‹ 返回） -->
      <div v-if="detailVisible && selectedItem" class="mobile-stack-layer">
        <MobileBackBar :title="selectedItem.name" @back="onMobileBack" />
        <div class="mobile-stack-body app-drawer-body">
          <AppDetailPanel
            :item="selectedItem"
            :busy="busyByPlugin[selectedItem.id] ?? ''"
            @back="onMobileBack"
            @install="installApp"
            @upgrade="upgradeApp"
            @uninstall="uninstallApp"
          />
        </div>
      </div>

      <!-- 「为本空间启用」：空间的应用市场（整页栈帧，同市场），只在空间上下文入口进入 -->
      <div v-else-if="view === 'enable'" class="mobile-stack-layer">
        <MobileBackBar title="应用市场 · 本空间" @back="onMobileBack" />
        <div class="mobile-stack-body">
          <SpaceMarketView />
        </div>
      </div>

      <!-- 应用市场：栈2 整页（返回栏 + 滚动内容），返回回应用列表（波次 4 整页化） -->
      <div v-else-if="view === 'market'" class="mobile-stack-layer">
        <MobileBackBar title="应用市场" @back="onMobileBack" />
        <div class="mobile-stack-body">
          <AppMarketPanel
            :items="visibleItems"
            @back="onMobileBack"
            @detail="(item) => openDetail(item)"
            @install="installApp"
            @install-repo="installRepoPlugin"
          />
        </div>
      </div>

      <AppListPanel
        v-else
        :installed-items="installedItems"
        :busy-by-plugin="busyByPlugin"
        @detail="(item) => openDetail(item)"
        @add-app="openMarket"
        @install-repo="installRepoPlugin"
        @sideloaded="refreshSafe"
      />
    </MobilePageTransition>

    <template v-else>
      <!-- 「为本空间启用」：空间的应用市场（页内切换，返回回系统层清单） -->
      <SpaceMarketView v-if="view === 'enable'" />

      <AppListPanel
        v-else-if="view === 'list'"
        :installed-items="installedItems"
        :busy-by-plugin="busyByPlugin"
        @detail="(item) => openDetail(item)"
        @add-app="openMarket"
        @install-repo="installRepoPlugin"
        @sideloaded="refreshSafe"
      />

      <AppMarketPanel
        v-else-if="view === 'market'"
        :items="visibleItems"
        @back="view = 'list'"
        @detail="(item) => openDetail(item)"
        @install="installApp"
        @install-repo="installRepoPlugin"
      />
    </template>

    <!-- 应用详情：全 app 统一抽屉（无头部小标题，右上角自定义关闭），不再整页切换；
         详情面板内的「返回」按钮同样映射为关闭抽屉；移动端（波次 2）不渲染抽屉，见上方整页层 -->
    <el-drawer v-if="!isMobileLayout" v-model="detailVisible" :with-header="false" size="520" class="app-drawer">
      <div class="app-drawer-body">
        <AppDetailPanel
          v-if="selectedItem"
          :item="selectedItem"
          :busy="busyByPlugin[selectedItem.id] ?? ''"
          @back="detailVisible = false"
          @install="installApp"
          @upgrade="upgradeApp"
          @uninstall="uninstallApp"
        />
      </div>
    </el-drawer>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketItemDto, RepoPluginDeclarationDto } from '../api/types';
import type { PropType } from 'vue';
import { currentSpace } from '../stores/current-space';
import { installPluginItem } from '../components/apps/app-actions';
import { isDevPlugin } from '../mock/dev-plugins';
import { isAdmin, refreshOrganizations } from '../stores/org-membership';
import { consumePendingAppDetail, consumePendingAppsView, pendingAppDetail, pendingAppsView } from '../stores/pending-app';
import { isMockApp, listMockApps, setMockAppInstalled } from '../mock/apps';
import { listDevPlugins } from '../mock/dev-plugins';
import { mockMode } from '../mock/mode';
import { spaceKeyOf } from '../mock/space-key';
import { notifyPluginInstalled, notifyPluginUpgraded } from '../plugin/messages';
import { isMobileLayout } from '../stores/ui-layout';
import { currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import MobileBackBar from '../components/MobileBackBar.vue';
import MobilePageTransition from '../components/MobilePageTransition.vue';
import AppListPanel from '../components/apps/AppListPanel.vue';
import AppMarketPanel from '../components/apps/AppMarketPanel.vue';
import AppDetailPanel from '../components/apps/AppDetailPanel.vue';
import SpaceMarketView from '../components/apps/SpaceMarketView.vue';
import { permissionLabel } from '../components/apps/apps-store';

export type OpenPluginTabPayload = {
  pluginDomain: string;
  pluginView: string;
  title: string;
  icon: string;
  pluginContext?: {
    orgId?: string;
  };
  /** 视图引导（事务深链等）：注入插件 window.__sparkPluginView.cardData */
  viewBootstrap?: { cardData?: unknown };
};

type ViewName = 'list' | 'market' | 'enable';
type BusyAction = '' | 'install' | 'upgrade' | 'toggle' | 'uninstall';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'apps';

export default defineComponent({
  name: 'AppsPage',
  props: { initialView: { type: String as PropType<ViewName>, default: 'list' } },
  components: { AppListPanel, AppMarketPanel, AppDetailPanel, SpaceMarketView, MobileBackBar, MobilePageTransition },
  emits: ['open-plugin-tab', 'changed'],
  setup(props, { emit }) {
    const items = ref<PluginMarketItemDto[]>([]);
    const realItems = ref<PluginMarketItemDto[]>([]);
    const loadError = ref('');
    const view = ref<ViewName>(props.initialView);
    /** 应用详情抽屉（点击卡片不再整页切换） */
    const detailVisible = ref(false);
    const selectedId = ref<string | null>(null);
    const busyByPlugin = ref<Record<string, BusyAction>>({});

    // 空间上下文：本页为系统层（列出全部已装应用，不按空间过滤，L4/M4）
    const isOrgSpace = computed(() => currentSpace.value.type === 'org');
    const spaceKey = computed(() =>
      currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    );

    // 是否当前组织管理员：org-membership 共享缓存的成员角色判断（§4.2/§5.2）。
    // 管理员角色仅用于组织空间的启用切换（装卸为系统层本机动作，不按角色设卡）；
    // 组织列表缓存同时供应用详情「空间启用情况」分区列出各组织空间
    const isCurrentUserAdmin = computed(() =>
      currentSpace.value.type === 'org' ? isAdmin(currentSpace.value.orgId) : false
    );
    const refreshAdminRole = async () => {
      try {
        await refreshOrganizations();
      } catch {
        // 读取失败保留旧缓存；无缓存时按非管理员展示
      }
    };

    /** 已安装清单＝系统层全量（L4/M4：应用管理不按空间过滤、不出现当前空间关系提示；
     *  「在哪些空间启用」进应用详情（只列已启用空间）；启用动作在空间的应用市场
     * （SpaceMarketView → 详情页 mode="space"，共享逻辑 app-actions.toggleAppEnablement），本页不操作启停） */
    const installedItems = computed(() => items.value.filter((item) => item.installed));

    /** 市场条目＝系统层全量（获取代码是本机动作，install-and-enable §一①；
     *  安装/启用权限与适用域在详情与启用环节体现，不在市场层过滤） */
    const visibleItems = computed(() => items.value);

    const selectedItem = computed(
      () => items.value.find((item) => item.id === selectedId.value) ?? null
    );

    // mock 模式（npm run tauri:mock）把 mock 应用（src/mock/apps.ts）合并进真实市场结果；
    // dev 链路（npm run dev / tauri:mock）把本地开发插件（src/mock/dev-plugins.ts，自动
    // 扫描 code/plugins/*/manifest.json）合并进来，生产构建不进 bundle（§5）。
    // 按 id 去重：同一插件的内核条目（内置包/仓库安装）优先于 dev 注入条目——
    // 否则同一插件会以两个条目（英文名内核条目 + 中文名 dev 条目）重复出现
    const mergeItems = () => {
      const merged = new Map<string, PluginMarketItemDto>();
      for (const item of listDevPlugins()) {
        merged.set(item.id, item);
      }
      if (mockMode()) {
        for (const item of listMockApps()) {
          merged.set(item.id, item);
        }
      }
      for (const item of realItems.value) {
        merged.set(item.id, item);
      }
      items.value = [...merged.values()];
      emit('changed');
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
      // 移动端（波次 2）：详情为整页栈帧，压入导航栈
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'detail', { id: item.id });
      }
    };

    /** 进入应用市场：移动端压入导航栈（整页 + 返回栏，波次 4 整页化）；桌面端页内视图切换 */
    const openMarket = () => {
      view.value = 'market';
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'market');
      }
    };

    /** 进入「为本空间启用」（空间层视图）：入口在空间上下文（桌面引导卡 / Launchpad 空态 /
        手机桌面「添加应用」瓦片）；移动端压入导航栈，桌面端页内切换 */
    const openEnable = () => {
      view.value = 'enable';
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'enable');
      }
    };

    // 移动端：栈顶帧变化（重进 tab 按栈恢复 / 返回 pop / 重按 tab 复位）时同步市场/详情显隐
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));
    watch(
      [mobileFrame, isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        if (frame.page === 'detail') {
          selectedId.value = frame.params?.id ?? null;
          detailVisible.value = true;
        } else {
          detailVisible.value = false;
          // 市场 / 为本空间启用整页帧与列表栈底帧同步到视图态（桌面端 view 由按钮直改，不受栈影响）
          view.value =
            frame.page === 'market' ? 'market' : frame.page === 'enable' ? 'enable' : 'list';
        }
      },
      { immediate: true }
    );

    /** 移动端返回栏 / 详情面板内返回：弹栈并收起详情整页（市场/列表态由栈帧 watch 同步） */
    const onMobileBack = () => {
      popPage(MOBILE_TAB);
      detailVisible.value = false;
    };

    // 消费「打开应用详情」请求（全局搜索跳转）：市场条目加载完成后找到该应用进入详情。
    // 详情属系统层（全量清单），不做空间可见性拦截——空间过滤只约束「打开/启用」等空间层动作
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

    const installApp = async (item: PluginMarketItemDto) => {
      // 安装是系统层本机动作（install-and-enable §一①：拿到代码≠任何空间启用），
      // 不做空间守卫；核心流程（权限确认→安装→通知）在共享动作 app-actions.installPluginItem
      setBusy(item.id, 'install');
      try {
        const installed = await installPluginItem(item);
        if (installed) {
          await refresh();
          mergeItems();
        }
      } finally {
        setBusy(item.id, '');
      }
    };

    // 仓库锚定安装（plugin-dist）：声明文件已在前置解析中展示，此处做权限确认后安装；
    // 同为系统层本机动作，不做空间守卫（supportedSpaces 仅约束启用，见 toggleEnabled）
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

    // 卸载：系统层本机动作（组织空间也不设卡——治理留给「启用」环节，install-and-enable §二）；
    // 确认框明示「仅移除插件程序，数据保留在本机」；已打开的插件 tab
    // 由 App.vue 监听 spark:close-plugin 事件联动关闭（复用 spark:open-* 同款事件模式）
    const uninstallApp = async (item: PluginMarketItemDto) => {
      // 本地开发插件：代码由 dev 链路直接服务、内核无安装记录，不可经市场卸载
      if (isDevPlugin(item.id)) {
        ElMessage.info('本地开发插件的代码在本机开发目录，不提供卸载（停止 dev 服务即不可用）');
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
      // mock 应用：无真实插件可卸，卸载只写 localStorage 状态（同安装口径）
      if (isMockApp(item)) {
        setMockAppInstalled(item.id, false);
        mergeItems();
        setBusy(item.id, '');
        ElMessage.success('应用已卸载');
        return;
      }
      try {
        await window.electronAPI.pluginMarket.uninstall(item.id);
        window.dispatchEvent(
          new CustomEvent('spark:close-plugin', { detail: { pluginDomain: item.domain } })
        );
        // refreshSafe 不抛错：卸载已成功，刷新失败只置 loadError，
        // 不能误报「卸载失败」（与 onMounted 初次加载同口径）
        await refreshSafe();
        ElMessage.success('应用已卸载');
      } catch (error) {
        ElMessage.error(`应用卸载失败：${error}`);
      } finally {
        setBusy(item.id, '');
      }
    };

    // 启用/停用已迁出：空间层动作唯一操作面＝空间的应用市场（SpaceMarketView，
    // 共享逻辑 components/apps/app-actions.ts 的 toggleAppEnablement，含先启用后安装、
    // 组织管理员守卫与内核回写）；本页（系统层应用管理）不操作启停

    // 切换空间时回到列表主视图并刷新组织缓存（管理员角色随空间而变）；
    // 移动端同步回栈底。清单本身为系统层全量、不随空间变化
    watch(spaceKey, () => {
      view.value = 'list';
      selectedId.value = null;
      resetStack(MOBILE_TAB);
      void refreshAdminRole();
    });

    onMounted(async () => {
      void refreshAdminRole();
      // 先展示应用列表：list() 是本地目录+安装态聚合，快；不等待网络更新探测。
      await refreshSafe();
      // 空间上下文入口（桌面引导卡 / 手机桌面「添加应用」）：直达「为本空间启用」视图
      if (consumePendingAppsView() === 'enable') {
        openEnable();
      }
      openPendingAppDetail();
      // 本地定期检测更新（设计 §4.4）：进入应用页时检测一次，发现新版本显示「可更新」角标。
      // checkUpdates 会对每个插件串行拉取 GitHub 远端 manifest+签名验签（connect 5s/总超时
      // 30s/个），网络差时可能很慢——改为后台异步执行，不阻塞首屏展示；完成后 refresh()
      // 让列表反映「可更新」角标。失败静默（同原语义，不阻断页面使用）。
      window.electronAPI.pluginMarket
        .checkUpdates()
        .then(() => refreshSafe())
        .catch(() => {});
    });

    return {
      items,
      visibleItems,
      loadError,
      view,
      detailVisible,
      selectedItem,
      busyByPlugin,
      isOrgSpace,
      isCurrentUserAdmin,
      installedItems,
      openDetail,
      installApp,
      installRepoPlugin,
      upgradeApp,
      uninstallApp,
      refreshSafe,
      isMobileLayout,
      MOBILE_TAB,
      openMarket,
      onMobileBack
    };
  }
});
</script>

<!-- 注意：不能加 scoped —— 列表/市场/详情卡片都在子组件（AppListPanel/AppMarketPanel/AppDetailPanel）内渲染，
     scoped 样式只会作用于本组件模板元素，无法穿透子组件，导致卡片样式整体失效（与 MessagesPage 等页面同样用非 scoped） -->
<style src="../styles/pages/apps.css"></style>
<style src="../styles/pages/apps-market.css"></style>
