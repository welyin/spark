<!-- PC 左栏入口的全局模态宿主（problem L12 / shell-desktop v0.7 §一 / ui-architecture v0.7 §4.1）：
     「全部消息 / 所有事务 / 应用管理 / 系统设置 / 我的 / 测试」统一以顶级对话框＋遮罩打开，
     打开形式同全局搜索（DesktopSearch，el-dialog 基准）：
       - 顶级 z-index（Element 弹层层级，盖住桌面窗口与 Dock）；
       - 半透明遮罩锁定背景（el-dialog modal 自带锁定），点遮罩 / Esc 关闭；
       - 单实例：任一时刻至多一个壳层模态（activeShellModal 互斥），重开同一入口即聚焦。
     例外不套模态：点「空间」里的具体空间项＝切换桌面；桌面应用图标＝桌面多窗口。
     仅桌面端渲染；手机端同各页面仍是全屏 tab（App.vue 移动分支），本组件不挂载。 -->
<template>
  <div v-if="!isMobileLayout" class="shell-modals">
    <el-dialog v-model="messagesVisible" title="全部消息" width="min(1120px, 94vw)" class="shell-modal">
      <div class="shell-modal-body">
        <BuiltinAppHost tab-id="messages" :space="pluginSpace" @fallback="onMessagesFallback">
          <template #legacy><MessagesPage /></template>
        </BuiltinAppHost>
      </div>
    </el-dialog>

    <el-dialog v-model="affairsVisible" title="所有事务" width="min(1080px, 94vw)" class="shell-modal">
      <div class="shell-modal-body"><AffairsPage /></div>
    </el-dialog>

    <el-dialog v-model="appsVisible" title="应用管理" width="min(1000px, 92vw)" class="shell-modal">
      <div class="shell-modal-body">
        <!-- 应用管理（A34 灰度，与消息同口径）：旧内置 UI ⇄ 默认内置插件版
             （spark-market；appsView==='market' 经 initial-view 透传直达市场页）。
             例外：'enable'（为本空间启用）是空间层视图，
             per-space 启停事实源在壳层 app-enablement（桥面无此数据面），
             插件版不承载，恒 legacy -->
        <BuiltinAppHost
          v-if="appsView !== 'enable'"
          tab-id="apps"
          :space="pluginSpace"
          :initial-view="appsView === 'market' ? 'market' : ''"
          @fallback="onAppsFallback"
        >
          <template #legacy>
            <AppsPage
              :key="appsView"
              :initial-view="appsView"
              @open-plugin-tab="onOpenPluginTab"
              @changed="refreshAppRegistry"
            />
          </template>
        </BuiltinAppHost>
        <AppsPage
          v-else
          :key="appsView"
          :initial-view="appsView"
          @open-plugin-tab="onOpenPluginTab"
          @changed="refreshAppRegistry"
        />
      </div>
    </el-dialog>

    <!-- 系统设置（L5）：弹窗只含系统设置（SystemSettingsPanel），个人设置已并入「我的」（L7） -->
    <el-dialog v-model="settingsVisible" title="系统设置" width="min(960px, 92vw)" class="shell-modal" @open="consumeSystemSection">
      <div class="shell-modal-body">
        <SystemSettingsPanel :key="settingsKey" :initial-section="settingsInitialSection ?? undefined" />
      </div>
    </el-dialog>

    <!-- MinePage 每次打开重建：重开回到「我的资料」首模块（否则对话框不关销毁，
         重开停留在上次离开的模块）；「我的」已改为根级页面、不随空间推导（走查修正） -->
    <el-dialog v-model="mineVisible" title="我的" width="min(1024px, 94vw)" class="shell-modal" @open="mineKey += 1">
      <div class="shell-modal-body"><MinePage :key="mineKey" @profile-updated="onProfileUpdated" /></div>
    </el-dialog>

    <el-dialog v-model="testVisible" title="测试" width="min(960px, 92vw)" class="shell-modal">
      <div class="shell-modal-body"><TestPage @back-root="closeShellModal" /></div>
    </el-dialog>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { currentSpace } from '../../stores/current-space';
import { refreshCurrentUser } from '../../stores/current-user';
import { setBuiltinImpl } from '../../stores/builtin-apps';
import { refreshAppRegistry } from '../../stores/desktop/app-registry';
import { consumePendingSystemSection, type SystemSectionKey } from '../../stores/pending-system-section';
import { isMobileLayout } from '../../stores/ui-layout';
import { activeShellModal, closeShellModal, shellModalOptions, type ShellModalId } from '../../stores/shell-modal';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import type { OpenPluginTabPayload } from '../../pages/AppsPage.vue';
import BuiltinAppHost from '../plugin/BuiltinAppHost.vue';
import MessagesPage from '../../pages/MessagesPage.vue';
import AffairsPage from '../../pages/AffairsPage.vue';
import AppsPage from '../../pages/AppsPage.vue';
import MinePage from '../../pages/MinePage.vue';
import TestPage from '../../pages/TestPage.vue';
import SystemSettingsPanel from '../settings/SystemSettingsPanel.vue';

export default defineComponent({
  name: 'ShellModals',
  components: { BuiltinAppHost, MessagesPage, AffairsPage, AppsPage, MinePage, TestPage, SystemSettingsPanel },
  emits: ['open-plugin-tab'],
  setup(_, { emit }) {
    /** 每个入口一个 el-dialog，可见性绑定到互斥单例 store（同一入口重开＝聚焦，不叠第二个） */
    const visibleOf = (id: ShellModalId) =>
      computed({
        get: () => activeShellModal.value === id,
        set: (value: boolean) => {
          if (!value && activeShellModal.value === id) closeShellModal();
        }
      });
    const messagesVisible = visibleOf('messages');
    const affairsVisible = visibleOf('affairs');
    const appsVisible = visibleOf('apps');
    const settingsVisible = visibleOf('settings');
    const mineVisible = visibleOf('mine');
    const testVisible = visibleOf('test');

    /** 应用管理初始视图：Dock「应用市场」经 openShellModal('apps', { appsView: 'market' }) 直达市场；
        桌面画布内入口（引导卡 / Launchpad 空态）经 appsView: 'enable' 直达「为本空间启用」（空间层视图） */
    const appsView = computed<'list' | 'market' | 'enable'>(() => shellModalOptions.appsView ?? 'list');

    /** 「我的」重建计数：每次打开 +1 强制重挂 MinePage（见模板注释，problem D2） */
    const mineKey = ref(0);

    /** 系统设置深链初始 section（网络状态点等经 requestOpenSystemSection 请求；打开时消费并重建面板） */
    const settingsInitialSection = ref<SystemSectionKey | null>(null);
    const settingsKey = ref(0);
    const consumeSystemSection = () => {
      const pending = consumePendingSystemSection();
      if (pending) {
        settingsInitialSection.value = pending;
        settingsKey.value += 1;
      }
    };

    /** 插件运行 space 上下文（透传 BuiltinAppHost；个人空间 id 恒 'personal'） */
    const pluginSpace = computed<PluginSpaceContext>(() => ({
      type: currentSpace.value.type,
      id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    }));

    const onMessagesFallback = () => setBuiltinImpl('messages', 'legacy');
    // 市场插件版加载失败/被关闭：回退该 tab 的旧内置 UI（与 messages 同口径灰度兜底）
    const onAppsFallback = () => setBuiltinImpl('apps', 'legacy');
    const onProfileUpdated = () => refreshCurrentUser();
    const onOpenPluginTab = (payload: OpenPluginTabPayload) => {
      // 应用管理里打开插件＝桌面窗口（桌面多窗口例外，不套模态）；先关掉模态让桌面露出
      closeShellModal();
      emit('open-plugin-tab', payload);
    };

    return {
      isMobileLayout,
      messagesVisible,
      affairsVisible,
      appsVisible,
      settingsVisible,
      mineVisible,
      testVisible,
      appsView,
      mineKey,
      settingsInitialSection,
      settingsKey,
      consumeSystemSection,
      pluginSpace,
      refreshAppRegistry,
      closeShellModal,
      onMessagesFallback,
      onAppsFallback,
      onProfileUpdated,
      onOpenPluginTab
    };
  }
});
</script>

<!-- SystemSettingsPanel 的栏位类（.mine-list/.mine-detail）定义在全局 mine.css（原由 SettingsPage 引入） -->
<style src="../../styles/pages/mine.css"></style>
<style src="../../styles/pages/settings.css"></style>

<style scoped>
/* 内容区：固定限高、内部各栏自行滚动（多栏页面均为 height:100% 的 flex 根） */
.shell-modal-body {
  display: flex;
  height: min(720px, 78vh);
  min-height: 0;
}

.shell-modal-body > * {
  flex: 1;
  min-width: 0;
  min-height: 0;
}

/* 壳层页面贴边多栏布局：去掉 el-dialog 默认 body 内边距 */
.shell-modal :deep(.el-dialog__body) {
  padding: 0;
}

.shell-modal :deep(.el-dialog__header) {
  margin-right: 0;
  border-bottom: 1px solid var(--spark-border-light);
}
</style>
