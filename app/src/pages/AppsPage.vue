<template>
  <section class="apps-page" :class="{ 'apps-page-mobile-stack': isMobileLayout && (detailVisible || view === 'market') }">
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
            :enabled="isEnabled(selectedItem)"
            :is-org-space="isOrgSpace"
            :is-admin="isCurrentUserAdmin"
            :busy="busyByPlugin[selectedItem.id] ?? ''"
            @back="onMobileBack"
            @open="openApp"
            @install="installApp"
            @upgrade="upgradeApp"
            @toggle="toggleEnabled"
            @uninstall="uninstallApp"
            @request-enable="requestEnable"
          />
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
        :recent-items="recentItems"
        :is-enabled="isEnabled"
        :is-suspended="isSuspended"
        :groups="groups"
        :is-org-space="isOrgSpace"
        :is-admin="isCurrentUserAdmin"
        :busy-by-plugin="busyByPlugin"
        @open="openApp"
        @detail="(item) => openDetail(item)"
        @toggle="toggleEnabled"
        @uninstall="uninstallApp"
        @add-app="openMarket"
        @install-repo="installRepoPlugin"
        @sideloaded="refreshSafe"
      />
    </MobilePageTransition>

    <template v-else>
      <AppListPanel
        v-if="view === 'list'"
        :installed-items="installedItems"
        :recent-items="recentItems"
        :is-enabled="isEnabled"
        :is-suspended="isSuspended"
        :groups="groups"
        :is-org-space="isOrgSpace"
        :is-admin="isCurrentUserAdmin"
        :busy-by-plugin="busyByPlugin"
        @open="openApp"
        @detail="(item) => openDetail(item)"
        @toggle="toggleEnabled"
        @uninstall="uninstallApp"
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
          @uninstall="uninstallApp"
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
import { isMobileLayout } from '../stores/ui-layout';
import { currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import MobileBackBar from '../components/MobileBackBar.vue';
import MobilePageTransition from '../components/MobilePageTransition.vue';
import AppListPanel from '../components/apps/AppListPanel.vue';
import AppMarketPanel from '../components/apps/AppMarketPanel.vue';
import AppDetailPanel from '../components/apps/AppDetailPanel.vue';
import {
  permissionLabel,
  useAppGroups,
  useOrgEnabled,
  useRecentApps
} from '../components/apps/apps-store';
import { isPluginVisibleInSpace } from '../components/apps/space-visibility';

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
type BusyAction = '' | 'install' | 'upgrade' | 'toggle' | 'uninstall';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'apps';

export default defineComponent({
  name: 'AppsPage',
  components: { AppListPanel, AppMarketPanel, AppDetailPanel, MobileBackBar, MobilePageTransition, Close },
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

    /** 插件按空间可见性（spaces-and-plugins §4）：supportedSpaces 缺省按 ['org']；
     *  仅 UI 展示过滤，已装但当前空间不可见的插件其会话/消息链路不受影响 */
    const isVisibleInCurrentSpace = (item: PluginMarketItemDto): boolean =>
      isPluginVisibleInSpace(item.supportedSpaces, currentSpace.value.type);

    /** 空间守卫提示（spaces-and-plugins §4）：打开/安装/详情直达各入口同一文案口径 */
    const spaceGuardToast = (name: string) => {
      ElMessage.warning(
        currentSpace.value.type === 'personal'
          ? `「${name}」仅支持组织空间，请切换到组织后使用`
          : `「${name}」仅支持个人空间，请切换到个人空间后使用`
      );
    };

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

    const installedItems = computed(() =>
      items.value.filter((item) => item.installed && isVisibleInCurrentSpace(item))
    );

    /** 市场收录层条目：与已安装列表同口径按当前空间过滤（纯组织插件不进个人空间市场） */
    const visibleItems = computed(() => items.value.filter(isVisibleInCurrentSpace));

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
          // 市场整页帧与列表栈底帧同步到视图态（桌面端 view 由按钮直改，不受栈影响）
          view.value = frame.page === 'market' ? 'market' : 'list';
        }
      },
      { immediate: true }
    );

    /** 移动端返回栏 / 详情面板内返回：弹栈并收起详情整页（市场/列表态由栈帧 watch 同步） */
    const onMobileBack = () => {
      popPage(MOBILE_TAB);
      detailVisible.value = false;
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
      // 空间守卫（spaces-and-plugins §4）：全局搜索已按空间过滤，此处兜底拦截
      // 过滤生效前派发的直达请求——toast 提示而非静默打开详情
      if (!isVisibleInCurrentSpace(item)) {
        spaceGuardToast(item.name);
        return;
      }
      openDetail(item);
    };
    watch([pendingAppDetail, items], openPendingAppDetail);

    const openApp = async (item: PluginMarketItemDto) => {
      // 空间直达守卫（spaces-and-plugins §4）：UI 已按空间过滤，此处兜底拦截
      // 经其他入口（如全局搜索详情页「打开」）触达的隐藏项
      if (!isVisibleInCurrentSpace(item)) {
        spaceGuardToast(item.name);
        return;
      }
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
      // 空间守卫（spaces-and-plugins §4）：与 openApp 同口径，拦截不可见项的安装直达
      if (!isVisibleInCurrentSpace(item)) {
        spaceGuardToast(item.name);
        return;
      }
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
      // 空间守卫（spaces-and-plugins §4）：声明文件 supportedSpaces 缺省按 ['org']
      if (!isPluginVisibleInSpace(declaration.supportedSpaces, currentSpace.value.type)) {
        spaceGuardToast(declaration.name);
        return;
      }
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

    // 卸载：确认框明示「仅移除插件程序，数据保留在本机」；已打开的插件 tab
    // 由 App.vue 监听 spark:close-plugin 事件联动关闭（复用 spark:open-* 同款事件模式）
    const uninstallApp = async (item: PluginMarketItemDto) => {
      // 组织空间权限守卫（与 toggleEnabled 的 isOrgSpace 口径一致）：
      // 仅管理员可卸载；入口按钮已按角色隐藏，此处兜底拒绝直达调用
      if (isOrgSpace.value && !isCurrentUserAdmin.value) {
        ElMessage.warning('只有组织管理员可以卸载应用');
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

    // 切换空间时回到列表主视图并刷新管理员角色；移动端同步回栈底（应用按空间隔离）
    watch(spaceKey, () => {
      view.value = 'list';
      selectedId.value = null;
      resetStack(MOBILE_TAB);
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
      visibleItems,
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
      uninstallApp,
      toggleEnabled,
      requestEnable,
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
