<template>
  <div
    class="shell"
    :class="{
      'shell-desktop': !isMobileLayout,
      'shell-rail-expanded': railExpanded,
      'shell-no-topbar': !isMobileLayout && activeTab !== 'space'
    }"
  >
    <!-- 顶部导航（最外层，横跨全宽）：
         桌面端=TopNavbar（左侧空间切换+中间搜索+右侧网络状态+「⋯」菜单）；
         移动端=MobileTopBar，仅四个主 tab 且栈深=1（一级列表页）时显示，
         切到二级页/其它 tab 时直接不渲染（无位移动画，Android 前端改造） -->
    <header
      v-if="isMobileLayout || activeTab === 'space'"
      class="topbar"
      :class="{ 'topbar-collapsed': isMobileLayout && !mobileTopBarVisible }"
    >
      <!-- 桌面端：顶栏（空间上下文条）仅在「空间」桌面出现；我的/消息/事务为整页无顶栏 -->
      <TopNavbar v-if="!isMobileLayout && activeTab === 'space'" />
      <MobileTopBar
        v-else-if="mobileTopBarVisible"
        :title="mobileTopBarTitle"
        @open-drawer="mobileDrawerVisible = true"
        @open-network-status="openNetworkStatus"
        @add-friend="onMobileAddContact('friend')"
        @add-member="onMobileAddContact('member')"
      />
    </header>

    <div class="shell-body">
      <!-- rail：顶部=当前身份头像（点击直达「我的资料」），中部=当前空间内的二级导航（ui-space-navbar §5）。
           两种状态：窄栏（64px，图标在上文字在下）/ 宽栏（155px，左图标右文字），
           点右侧分隔线（col-resize 光标）切换，选择持久化在 localStorage；
           窄屏（≤768px）下不渲染，导航由底部 MobileTabBar 接管（ui-layout 断点） -->
      <nav v-if="!isMobileLayout" class="rail" :class="{ expanded: railExpanded }">
        <div class="rail-main">
          <template v-for="item in navItems" :key="item.id">
          <button
            class="rail-item"
            :title="item.label"
            :class="{ active: isNavActive(item.id) }"
            @click="item.id === 'search' ? openDesktopSearch() : handleMenuSelect(item.id)"
          >
            <!-- 消息入口挂当前空间未读总数角标（免打扰不计入，>99 显示 99+），品牌红 -->
            <el-badge v-if="item.id === 'messages'" :value="messagesBadge" :max="99" :hidden="messagesBadge === 0">
              <el-icon :size="20"><component :is="item.icon" /></el-icon>
            </el-badge>
            <!-- 事务入口挂「待我处理」角标（G7，非全部进行中数） -->
            <el-badge v-else-if="item.id === 'affairs'" :value="affairsBadge" :max="99" :hidden="affairsBadge === 0">
              <el-icon :size="20"><component :is="item.icon" /></el-icon>
            </el-badge>
            <el-icon v-else :size="20"><component :is="item.icon" /></el-icon>
            <span class="rail-label">{{ item.label }}</span>
            <!-- 空间行右侧：「＋」创建/加入组织菜单（弹层内容同原二级菜单底部入口，用户评审决策） -->
            <el-dropdown
              v-if="item.id === 'space'"
              trigger="click"
              placement="right-start"
              @command="onSpaceMembershipCommand"
            >
              <span class="rail-space-plus" title="创建 / 加入组织" @click.stop>
                <el-icon :size="13"><Plus /></el-icon>
              </span>
              <template #dropdown>
                <el-dropdown-menu>
                  <el-dropdown-item command="create">创建组织</el-dropdown-item>
                  <el-dropdown-item command="join">加入组织</el-dropdown-item>
                </el-dropdown-menu>
              </template>
            </el-dropdown>
          </button>

          <!-- 「空间」二级列表：宽栏下常驻展示（用户决策），不随当前 tab 显隐 -->
          <RailSpaceList
            v-if="item.id === 'space' && railExpanded"
            ref="railSpaceListRef"
          />
          </template>
        </div>

        <div class="rail-divider" aria-hidden="true" />
        <div class="rail-bottom">
          <!-- 设置 / 测试：从「⋯」更多菜单拿出，固定显示在「我的」上方（用户评审决策 2026-09-09） -->
          <button
            class="rail-item"
            title="设置"
            :class="{ active: activeTab === 'settings' }"
            @click="handleMenuSelect('settings')"
          >
            <el-icon :size="20"><Setting /></el-icon>
            <span class="rail-label">设置</span>
          </button>
          <button
            v-if="isDevelopment"
            class="rail-item"
            title="测试"
            :class="{ active: activeTab === 'test' }"
            @click="handleMenuSelect('test')"
          >
            <el-icon :size="20"><Cpu /></el-icon>
            <span class="rail-label">测试</span>
          </button>
          <!-- 当前身份（=「我的」入口）：头像 + 名字，名字下方为网络状态行；
               右侧「⋯」更多菜单（切换账号/退出登录；设置/测试已拿出为独立按钮） -->
          <div class="rail-identity">
            <UserAvatarMenu :avatar-size="36" @open-profile="handleMenuSelect('mine')">
              <template #subtitle><NetworkStatusBar variant="line" /></template>
            </UserAvatarMenu>
            <el-dropdown v-if="railExpanded" trigger="click" @command="onRailMoreCommand">
              <button class="rail-more" title="更多">
                <el-icon :size="16"><MoreFilled /></el-icon>
              </button>
              <template #dropdown>
                <el-dropdown-menu>
                  <el-dropdown-item command="switch-account">切换账号</el-dropdown-item>
                  <el-dropdown-item command="logout" class="rail-more-danger">退出登录</el-dropdown-item>
                </el-dropdown-menu>
              </template>
            </el-dropdown>
          </div>
          <NetworkStatusBar v-if="!railExpanded" compact />
        </div>

        <!-- 宽窄切换把手：覆盖在右侧 2px 分隔线上（热区 10px，col-resize 光标），点击切换 rail 状态 -->
        <div
          class="rail-resizer"
          :title="railExpanded ? '收起导航栏' : '展开导航栏'"
          @click="toggleRail"
        />
      </nav>

      <main class="main">
        <!-- 移动端（波次 3）：tab 切换主区域短淡入淡出（150ms），底部 tab bar 自身不动；
             栈内 push/pop 转场在各页面内部（MobilePageTransition） -->
        <Transition v-if="isMobileLayout" name="mobile-tab-fade">
          <div :key="activeTab" class="mobile-tab-page">
            <!-- 聊天：灰度开关（旧内置 UI ⇄ 默认内置插件版，stores/builtin-apps）；
                 通讯录已转为空间桌面插件窗口（docs/ui 阶段 3），不再有壳层主 tab 灰度面 -->
            <BuiltinAppHost
              v-if="activeTab === 'messages'"
              tab-id="messages"
              :space="pluginSpace"
              @fallback="onBuiltinPluginClose('messages')"
            >
              <template #legacy><MessagesPage /></template>
            </BuiltinAppHost>
            <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
            <!-- 手机端空间（阶段 1）：两级结构 域列表→域桌面；点图标 open-app 走 openPluginTab 全屏 App -->
            <SpaceMobile v-else-if="activeTab === 'space'" @open-app="openPluginTab" @open-market="handleMenuSelect('apps')" />
            <!-- 事务（阶段 3，移动端共用 AffairsPage 列表形态） -->
            <AffairsPage v-else-if="activeTab === 'affairs'" />
            <TestPage v-else-if="activeTab === 'test'" @back-root="backFromSecondaryTab('test')" />
            <SettingsPage
              v-else-if="activeTab === 'settings'"
              @profile-updated="loadCurrentUser"
              @open-tab="handleMenuSelect"
              @back-root="backFromSecondaryTab('settings')"
            />
            <!-- 「我的资料」隐藏入口：点击 rail 顶部头像进入，不在 rail 展示 -->
            <MinePage v-else-if="activeTab === 'mine'" @profile-updated="loadCurrentUser" />

            <el-card v-else-if="activePluginTab" shadow="never" class="plugin-tab-card" :class="{ 'plugin-tab-card--immersive': !pluginHostTitleBar }">
              <template v-if="pluginHostTitleBar" #header>
                <div class="plugin-tab-header-bar">
                  <div class="plugin-tab-header-left">
                    <el-button text type="primary" @click="goBackFromPlugin">&lt; 返回</el-button>
                  </div>
                  <div class="plugin-tab-header-center">
                    <h1>{{ activePluginTab.title }}</h1>
                    <p>{{ activePluginTab.pluginDomain }} / {{ activePluginTab.pluginView }}</p>
                  </div>
                  <div class="plugin-tab-header-right" />
                </div>
              </template>
              <!-- 插件自接管顶栏（chrome.hostTitleBar:false）：head 隐藏、iframe 全高，
                   返回按钮由插件自己在内部实现（sdk.close() 请求关闭） -->
              <PluginIframeHost
                :key="`${activePluginTab.id}|${pluginSpace.id}`"
                :plugin-id="activePluginTab.pluginDomain.slice('plugin:'.length)"
                :view-id="activePluginTab.pluginView"
                :space="pluginSpace"
                :view-bootstrap="activePluginTab.viewBootstrap"
                @close="closePluginTab"
                @manifest="onPluginManifest"
              />
            </el-card>
          </div>
        </Transition>

        <!-- 桌面端（≥769px）：渲染逻辑不变，无任何切换动画 -->
        <template v-else>
          <!-- 聊天/通讯录：灰度开关（旧内置 UI ⇄ 默认内置插件版，stores/builtin-apps） -->
          <BuiltinAppHost
            v-if="activeTab === 'messages'"
            tab-id="messages"
            :space="pluginSpace"
            @fallback="onBuiltinPluginClose('messages')"
          >
            <template #legacy><MessagesPage /></template>
          </BuiltinAppHost>
          <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
          <!-- 桌面端空间（阶段 2）：PC 多窗口桌面；移动端走上方 SpaceMobile -->
          <!-- 事务（阶段 3）：跨域「与我相关」事务列表；点卡片按类型分发到类型插件 -->
          <AffairsPage v-else-if="activeTab === 'affairs'" />
          <TestPage v-else-if="activeTab === 'test'" @back-root="backFromSecondaryTab('test')" />
          <SettingsPage
            v-else-if="activeTab === 'settings'"
            @profile-updated="loadCurrentUser"
            @open-tab="handleMenuSelect"
            @back-root="backFromSecondaryTab('settings')"
          />
          <!-- 「我的资料」隐藏入口：点击 rail 顶部头像进入，不在 rail 展示 -->
          <MinePage v-else-if="activeTab === 'mine'" @profile-updated="loadCurrentUser" />

          <el-card v-else-if="activePluginTab" shadow="never" class="plugin-tab-card" :class="{ 'plugin-tab-card--immersive': !pluginHostTitleBar }">
            <!-- 插件自接管顶栏（chrome.hostTitleBar:false）：header 内容置空、el-card 不渲染 head，
                 iframe 全高，仅左上角悬浮返回图标叠在 iframe 之上 -->
            <template v-if="pluginHostTitleBar" #header>
              <div class="plugin-tab-header-bar">
                <div class="plugin-tab-header-left">
                  <el-button text type="primary" @click="goBackFromPlugin">&lt; 返回</el-button>
                </div>
                <div class="plugin-tab-header-center">
                  <h1>{{ activePluginTab.title }}</h1>
                  <p>{{ activePluginTab.pluginDomain }} / {{ activePluginTab.pluginView }}</p>
                </div>
                <div class="plugin-tab-header-right" />
              </div>
            </template>
            <!-- iframe 沙箱运行时（插件加载唯一路径，阶段 A 第三波起）：
                 独立 origin iframe + postMessage 桥 + 权限中间件 + 心跳熔断；
                 space 切换经 :key 重建实例 -->
            <PluginIframeHost
              :key="`${activePluginTab.id}|${pluginSpace.id}`"
              :plugin-id="activePluginTab.pluginDomain.slice('plugin:'.length)"
              :view-id="activePluginTab.pluginView"
              :space="pluginSpace"
              :view-bootstrap="activePluginTab.viewBootstrap"
              @close="closePluginTab"
              @manifest="onPluginManifest"
            />
          </el-card>
          <PcDesktop v-if="desktopVisited" v-show="activeTab === 'space'" @open-market="handleMenuSelect('apps')" @open-app="openPluginTab" />
        </template>
      </main>
    </div>

    <!-- 移动端底部 tab 导航：窄屏（≤768px）替代左侧 rail，与 rail 共用 activeTab 状态源；
         仅四个主 tab 且栈深=1（一级页）时显示——进入二级页时直接不渲染、返回时直接出现
         （Android 前端改造，无位移动画） -->
    <MobileTabBar
      v-if="mobileTabBarVisible"
      :active-tab="activeTab"
      :messages-badge="messagesBadge"
      @select="handleMenuSelect"
    />

    <!-- 移动端左滑侧边栏（Android 前端改造）：空间切换 + 加入/创建 + 设置；「⋯」菜单已下放此处与设置页 -->
    <MobileSpaceDrawer
      v-if="isMobileLayout"
      v-model="mobileDrawerVisible"
      @open-settings="handleMenuSelect('settings')"
    />

    <DesktopSearch v-if="!isMobileLayout" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { onBackButtonPress } from '@tauri-apps/api/app';
import { listen as listenTauriEvent } from '@tauri-apps/api/event';
import { ChatDotRound, Cpu, Document, Grid, MoreFilled, Notebook, Setting, Search, Plus } from '@element-plus/icons-vue';
import DesktopSearch from './components/desktop/DesktopSearch.vue';
import NetworkStatusBar from './components/NetworkStatusBar.vue';
import { activeWinKey, closeAppWindows, openWindow, openNewWindow, windows as desktopWindows } from './stores/desktop/window-manager';
import { getApp, refreshAppRegistry } from './stores/desktop/app-registry';
import type { PluginManifest, PluginSpaceContext } from '../../packages/plugin-sdk/src';
import PluginIframeHost from './components/plugin/PluginIframeHost.vue';
import BuiltinAppHost from './components/plugin/BuiltinAppHost.vue';
import { setBuiltinImpl } from './stores/builtin-apps';
import { fetchPluginManifest } from './plugin/source';
import { unreadCountOf } from './stores/messages';
import { spaceKeyOf } from './mock/contacts';
import { actionableCount, refreshAffairFeed } from './stores/affairs/affair-feed';
import { OPEN_PLUGIN_DEEPLINK_EVENT, openPluginDeepLink, type OpenPluginDeepLink } from './services/deep-link';
import {
  currentSpace,
  validateCurrentSpace
} from './stores/current-space';
import { refreshCurrentUser, currentUser } from './stores/current-user';
import { refreshProfileExtraFromKernel } from './stores/profile-extra';
import { refreshOrgIdentity } from './stores/org-identity';
import { refreshOrganizations } from './stores/org-membership';
import { requestOpenChat } from './stores/pending-chat';
import { type AddContactKind } from './stores/pending-add-contact';
import { CONTACT_INTENT_ADD } from './components/contacts/open-intents';
import { requestOpenAppDetail } from './stores/pending-app';
import TopNavbar from './components/TopNavbar.vue';
import UserAvatarMenu from './components/UserAvatarMenu.vue';
import MobileTabBar from './components/MobileTabBar.vue';
import MobileTopBar from './components/MobileTopBar.vue';
import MobileSpaceDrawer from './components/MobileSpaceDrawer.vue';
import { isMobileLayout, MOBILE_TABS } from './stores/ui-layout';
import { listenP2pEvents, isTauri } from './api';
import { initNotify } from './stores/notify';
import { currentPage, popPage, resetStack } from './stores/mobile-nav';
import { hasOverlay, requestCloseOverlay } from './stores/overlay-stack';
import { requestOpenSystemSection } from './stores/pending-system-section';
import { handleDeviceNotice, hydrateDeviceNotices } from './stores/device-notices';
import { notifyDeviceJoined } from './plugin/messages';
import { handleRecoveryP2pEvent, hydrateRecovery } from './stores/recovery';
import { handlePasswordUnifyEvent, hydratePasswordUnify } from './stores/password-unify';
import { useUpdaterReadyPrompt } from './components/updater/use-updater';
import { lockAndReload } from './utils/identity-lock';
import MessagesPage from './pages/MessagesPage.vue';

import AppsPage, { type OpenPluginTabPayload } from './pages/AppsPage.vue';
import TestPage from './pages/TestPage.vue';
import SettingsPage from './pages/SettingsPage.vue';
import MinePage from './pages/MinePage.vue';
import PlaceholderPage from './pages/PlaceholderPage.vue';
import AffairsPage from './pages/AffairsPage.vue';
import SpaceMobile from './components/SpaceMobile.vue';
import PcDesktop from './components/desktop/PcDesktop.vue';
import RailSpaceList from './components/desktop/RailSpaceList.vue';

type PluginTab = {
  id: string;
  pluginDomain: string;
  pluginView: string;
  title: string;
  icon: string;
  sourceTab?: string;
  pluginContext?: {
    orgId?: string;
  };
  /** 视图引导（事务深链等）：注入插件 window.__sparkPluginView.cardData（如 affairId） */
  viewBootstrap?: { cardData?: unknown };
};

export default defineComponent({
  name: 'App',
  components: {
    DesktopSearch,
    NetworkStatusBar,
    Search,
    MessagesPage,
    AppsPage,
    TestPage,
    SettingsPage,
    MinePage,
    PlaceholderPage,
    AffairsPage,
    SpaceMobile,
    PcDesktop,
    RailSpaceList,
    BuiltinAppHost,
    TopNavbar,
    UserAvatarMenu,
    MobileTabBar,
    MobileTopBar,
    MobileSpaceDrawer,
    PluginIframeHost,
    ChatDotRound,
    Notebook,
    Grid,
    Document,
    Cpu,
    Setting,
    Plus,
    MoreFilled
  },
  setup() {
    const openDesktopSearch = () => window.dispatchEvent(new Event('spark:search'));
    /** 空间行右侧「＋」菜单：转发给 RailSpaceList 的创建/加入对话框（经模板 ref） */
    const railSpaceListRef = ref<InstanceType<typeof RailSpaceList> | null>(null);
    const onSpaceMembershipCommand = (command: string) => {
      railSpaceListRef.value?.openMembership(command === 'create' ? 'create' : 'join');
    };
    // rail 底部身份块右侧「⋯」菜单（原顶栏菜单迁入）：设置/测试切 tab，切换账号/退出走锁定重载
    const isDevelopment = import.meta.env.DEV;
    const onRailMoreCommand = (command: string) => {
      if (command === 'settings' || command === 'test') {
        handleMenuSelect(command);
        return;
      }
      if (command === 'logout') {
        handleLogout();
      } else if (command === 'switch-account') {
        handleSwitchAccount();
      }
    };
    // PC：桌面常驻（activeTab 恒为 space，壳层页面全部窗口化）；移动端仍为 tab 切页
    const activeTab = ref<string>(isMobileLayout.value ? 'messages' : 'space');
    const desktopVisited = ref(activeTab.value === 'space');
    watch(activeTab, (tab) => { if (tab === 'space') desktopVisited.value = true; });
    // 移动端左滑侧边栏可见性（Android 前端改造）
    const mobileDrawerVisible = ref(false);
    const pluginTabs = ref<PluginTab[]>([]);
    // rail 宽窄状态（持久化）：false=64px 窄栏，true=155px 宽栏。
    // 新 UI（shell-desktop §六决策 1）默认展开 240px 宽栏，用户可折叠、选择被记忆
    const railExpanded = ref(localStorage.getItem('spark:rail-expanded') !== '0');
    const toggleRail = () => {
      railExpanded.value = !railExpanded.value;
      localStorage.setItem('spark:rail-expanded', railExpanded.value ? '1' : '0');
    };
    // 当前登录用户资料：stores/current-user 单例（rail 头像/空间切换器/消息气泡自己头像共用）；
    // 主窗口挂载时读取一次，资料更新（profile-updated）后重新读取
    const loadCurrentUser = refreshCurrentUser;

    // rail 一级入口：搜索（浮层）· 全部消息 · 空间 · 所有事务；
    // PC 上消息/事务是跨空间全局窗口，在当前桌面弹出；「我的」入口 = 底部头像
    const navItems = [
      { id: 'search', label: '搜索', icon: Search },
      { id: 'messages', label: '全部消息', icon: ChatDotRound },
      { id: 'space', label: '空间', icon: Grid },
      { id: 'affairs', label: '所有事务', icon: Document }
    ];

    /** 壳层页面 → 桌面窗口 id（PC 上这些 tab 不再整页切换，在当前空间桌面开窗） */
    const SHELL_WINDOW_TABS: Record<string, string> = {
      messages: 'spark:messages',
      affairs: 'spark:affairs',
      mine: 'spark:mine',
      settings: 'spark:settings',
      test: 'spark:test',
      apps: 'spark:market'
    };

    /** rail 高亮：空间恒亮（桌面常驻）；消息/事务看前台窗口是否为对应壳窗 */
    const isNavActive = (id: string) => {
      if (isMobileLayout.value) return activeTab.value === id;
      if (id === 'space') return activeTab.value === 'space';
      const shellId = SHELL_WINDOW_TABS[id];
      if (!shellId) return false;
      const front = desktopWindows.value.find((w) => w.key === activeWinKey.value);
      return !!front && !front.minimized && front.appId === shellId;
    };

    /** 移动端顶部导航仅四个主 tab（底部 tab 对应的一级页）显示；进入二级页（栈深>1）时整体隐藏 */
    const MAIN_TAB_IDS: string[] = MOBILE_TABS.map((tab) => tab.id);

    /** 一级页判定：移动端 + 四个主 tab + 栈底帧（page==='root'）。顶栏与底部 tab 共用此条件，
        进入二级页（聊天/详情/模块）时两者一并隐藏（Android 前端改造） */
    const isPrimaryPage = computed(() => {
      if (!isMobileLayout.value) {
        return false;
      }
      if (!MAIN_TAB_IDS.includes(activeTab.value)) {
        return false;
      }
      return currentPage(activeTab.value).page === 'root';
    });

    /** 移动端顶部导航可见性：仅一级页显示 */
    const mobileTopBarVisible = isPrimaryPage;

    /** 移动端底部 tab 导航可见性：仅一级页显示（二级页随首页一并推走） */
    const mobileTabBarVisible = isPrimaryPage;

    /** 顶部导航中间页名（消息/通讯录/应用/我的） */
    const mobileTopBarTitle = computed(() => {
      return MOBILE_TABS.find((tab) => tab.id === activeTab.value)?.label ?? '';
    });

    /** 网络状态点点击：切到设置页并直达「系统设置→网络状态」（Android 顶部导航改造）。
        走 handleMenuSelect：统一记录来源 tab 并把设置栈重置到栈底（深链再压入分组页帧） */
    const openNetworkStatus = () => {
      requestOpenSystemSection('netStatus');
      handleMenuSelect('settings');
    };

    /** 移动端顶栏「+」菜单：经统一深链打开空间通讯录插件的添加对话框（0.3：
        通讯录是空间插件，docs/ui §八决策 1；cardData.intent=__add__ 由插件侧消费，
        插件按当前空间区分「添加朋友/添加成员」） */
    const onMobileAddContact = (kind: AddContactKind) => {
      openPluginDeepLink({ pluginId: 'spark-contacts', cardData: { intent: CONTACT_INTENT_ADD, kind } });
    };

    /** 消息入口角标：当前空间未读消息总数（按空间隔离） */
    const messagesBadge = computed(() => unreadCountOf(spaceKeyOf(currentSpace.value)));

    /** 事务入口角标（G7）：「待我处理」数（affair-feed 近似：进行中且我关注，ui-architecture §七风险 4） */
    const affairsBadge = computed(() => actionableCount.value);

    const activePluginTab = computed(() => {
      return pluginTabs.value.find((tab) => tab.id === activeTab.value) ?? null;
    });

    /** 当前插件 manifest（PluginIframeHost 透传）：用于顶栏按 chrome.hostTitleBar 决策 */
    const pluginManifest = ref<PluginManifest | null>(null);
    const onPluginManifest = (manifest: PluginManifest | null) => {
      pluginManifest.value = manifest;
    };
    /** 插件自接管顶栏（壳层隐藏默认顶栏主体、仅左上角保留悬浮返回图标） */
    const pluginHostTitleBar = computed(
      () => pluginManifest.value?.chrome?.hostTitleBar !== false
    );

    /** 插件运行 space 上下文（透传 PluginIframeHost；个人空间 id 恒 'personal'） */
    const pluginSpace = computed<PluginSpaceContext>(() => ({
      type: currentSpace.value.type,
      id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    }));

    /** 关闭当前插件 tab（熔断覆盖层「关闭」；移除 tab 并回来源页） */
    const closePluginTab = () => {
      const tab = activePluginTab.value;
      if (!tab) {
        return;
      }
      pluginTabs.value = pluginTabs.value.filter((item) => item.id !== tab.id);
      activeTab.value = tab.sourceTab ?? 'apps';
    };

    /** 卸载插件后联动关闭其全部已打开 tab（AppsPage 卸载成功时派发 spark:close-plugin） */
    const onClosePluginEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ pluginDomain?: string }>).detail;
      if (!detail?.pluginDomain) {
        return;
      }
      closeAppWindows(detail.pluginDomain.replace(/^plugin:/, ''));
      void refreshAppRegistry();
      const closing = pluginTabs.value.filter((tab) => tab.pluginDomain === detail.pluginDomain);
      if (closing.length === 0) {
        return;
      }
      pluginTabs.value = pluginTabs.value.filter((tab) => tab.pluginDomain !== detail.pluginDomain);
      // 当前正停留在被关闭的 tab：回其来源页（缺省回应用页）
      const active = closing.find((tab) => tab.id === activeTab.value);
      if (active) {
        activeTab.value = active.sourceTab ?? 'apps';
      }
    };

    // 非主 tab（设置/测试等无底部导航的页面）的来源记录（Android 前端改造）：
    // 根页返回（UI 返回按钮与系统返回键）据此回到来源页；无记录时缺省回消息页
    const secondaryTabReturnTo: Record<string, string> = {};

    const handleMenuSelect = (index: string) => {
      // PC：壳层页面统一窗口化——桌面常驻，在当前空间桌面弹出对应窗口
      if (!isMobileLayout.value && SHELL_WINDOW_TABS[index]) {
        activeTab.value = 'space';
        openWindow(SHELL_WINDOW_TABS[index]);
        return;
      }
      // 移动端（波次 2/Android 改造）：切 tab 一律回到该 tab 导航栈底（列表页）——
      // 底部导航切换应展示该页的首页内容，而非离开时停留的二级页
      if (isMobileLayout.value) {
        resetStack(index);
        // 进入非主 tab 前记录来源（供该页根页返回使用）
        if (!MAIN_TAB_IDS.includes(index) && !index.startsWith('plugin|') && index !== activeTab.value) {
          secondaryTabReturnTo[index] = activeTab.value;
        }
      }
      activeTab.value = index;
    };

    /** 非主 tab 根页返回：回到来源 tab（设置/测试等页面共用；缺省回消息页） */
    const backFromSecondaryTab = (tabId?: string) => {
      const tab = tabId ?? activeTab.value;
      const target = secondaryTabReturnTo[tab] ?? 'messages';
      activeTab.value = target;
      if (isMobileLayout.value) {
        resetStack(target);
      }
    };

    const openPluginTab = async (payload: OpenPluginTabPayload) => {
      const pluginDomain = payload.pluginDomain.trim();
      const pluginView = payload.pluginView.trim() || 'default';
      if (!pluginDomain.startsWith('plugin:')) {
        ElMessage.error(`无效插件域：${pluginDomain}`);
        return;
      }

      if (!isMobileLayout.value) {
        await refreshAppRegistry();
        const appId = pluginDomain.slice('plugin:'.length);
        if (!getApp(appId)) {
          ElMessage.warning('该应用未安装或不支持当前空间');
          return;
        }
        activeTab.value = 'space';
        if (payload.viewBootstrap || pluginView !== getApp(appId)?.view) {
          openNewWindow(appId, { viewId: pluginView, viewBootstrap: payload.viewBootstrap });
        } else {
          openWindow(appId);
        }
        return;
      }

      const pluginContext = payload.pluginContext;
      const contextSuffix = pluginContext?.orgId ? `|${pluginContext.orgId}` : '';

      const tabId = `plugin|${pluginDomain}|${pluginView}${contextSuffix}`;
      const existing = pluginTabs.value.find((item) => item.id === tabId);
      if (!existing) {
        const sourceTab = activeTab.value.startsWith('plugin|') ? 'apps' : activeTab.value;
        pluginTabs.value.push({
          id: tabId,
          pluginDomain,
          pluginView,
          title: payload.title || `${pluginDomain}/${pluginView}`,
          icon: payload.icon || 'P',
          sourceTab,
          pluginContext,
          viewBootstrap: payload.viewBootstrap
        });
      }
      // 切插件前重置 manifest（新插件顶栏决策待 PluginIframeHost 重新上报）
      pluginManifest.value = null;
      activeTab.value = tabId;
    };

    const goBackFromPlugin = () => {
      const tab = activePluginTab.value;
      const fallback = 'apps';
      activeTab.value = tab?.sourceTab ?? fallback;
    };

    /** 默认内置插件版加载失败/被关闭：回退该 tab 的旧内置 UI（灰度兜底，开关持久化回 legacy） */
    const onBuiltinPluginClose = (tabId: string) => {
      setBuiltinImpl(tabId, 'legacy');
    };

    // 锁定身份后整窗重载回登录/选择账号页（RootGate 接管）；统一走 identity-lock 收敛点
    const handleLogout = () => lockAndReload('已退出登录');
    const handleSwitchAccount = () => lockAndReload('已退出当前账号');

    // 通讯录/应用市场请求打开 1:1 会话（ui-contacts §5.3）：记录请求并切到消息页
    const onOpenChatEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ rootId?: string; name?: string; conversationId?: string }>).detail;
      if (!detail?.rootId) {
        return;
      }
      requestOpenChat({ rootId: detail.rootId, name: detail.name ?? '', conversationId: detail.conversationId });
      handleMenuSelect('messages');
    };

    // 全局搜索请求打开应用详情：记录请求并切到应用页（AppsPage 消费）
    const onOpenAppEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ id?: string }>).detail;
      if (!detail?.id) {
        return;
      }
      requestOpenAppDetail(detail.id);
      handleMenuSelect('apps');
    };

    // 设置页「个人资料」入口卡请求打开个人设置（PC 开窗，移动切 tab）
    const onOpenMineEvent = () => {
      handleMenuSelect('mine');
    };

    // 统一深链（services/deep-link）：消息卡片回退 / 事务分发 / 通知中心共用——
    // 查 pluginMarket 拿 domain → openPluginTab 渲染 PluginIframeHost，viewBootstrap.cardData 注入插件
    const onOpenPluginDeepLink = async (event: Event) => {
      const detail = (event as CustomEvent<OpenPluginDeepLink>).detail;
      if (!detail?.pluginId) {
        return;
      }
      try {
        const items = await window.electronAPI.pluginMarket.list();
        const item = items.find((entry) => entry.id === detail.pluginId);
        if (!item) {
          return;
        }
        openPluginTab({
          pluginDomain: item.domain,
          pluginView: detail.viewId ?? item.views[0] ?? 'default',
          title: item.name,
          icon: item.name.slice(0, 1),
          pluginContext: currentSpace.value.type === 'org' ? { orgId: currentSpace.value.orgId } : undefined,
          // 深链：cardData（如 affairId）经 viewBootstrap 注入插件（主视图读取定位详情）
          viewBootstrap: detail.cardData ? { cardData: detail.cardData } : undefined
        });
      } catch {
        // 插件清单读取失败静默（调用方已给未装引导）
      }
    };

    // p2p 事件退订器收集（SelfProfileSynced 等；卸载时统一退订）
    const unlistenP2p: Array<() => void> = [];

    onMounted(() => {
      void loadCurrentUser().then(() => {
        // 初次水合组织身份（内核成员字段覆盖 localStorage 种子缓存）
        refreshOrgIdentity();
        // M1 新设备通知：水合当前身份的待看通知（重启后红点恢复）
        hydrateDeviceNotices(currentUser.rootId ?? '');
        // M5 延迟恢复：水合本机 pending + 持久化的 inbound
        hydrateRecovery(currentUser.rootId ?? '');
        // 乙+校验器：水合本机 pendingUnify 状态
        hydratePasswordUnify(currentUser.rootId ?? '');
      });
      // 懒校验启动恢复的组织空间：组织已不存在时回退个人空间
      void validateCurrentSpace();
      // 插件后台运行时对账（内核 QuickJS 沙箱）：身份切换会停全部插件后台，
      // 进入主界面按当前身份重新拉起（幂等）
      window.electronAPI?.pluginRuntime?.syncBackgrounds().catch(() => {});
      // 阶段四C：系统通知编排（Android 真弹/桌面 no-op；判流在 stores/notify）
      initNotify();
      // 自设备资料同步（多设备）：本机资料被其他设备的全量快照更新后刷新展示
      void listenP2pEvents((event) => {
        // M1 新设备加入通知（m1-m2-implementation-plan §3.4）：store 按 deviceId 幂等
        // （重复事件仅更新 ts 不重复写）；红旗引导文案已拍板保留（方案 §8 决策点 1）。
        // 通知改走消息页 app:system 系统消息落一条可追溯记录（不弹 tips），
        // 写入当前空间，消息入口角标随之 +1；红点数据源（stores/device-notices）保留。
        if (event.kind === 'DeviceNoticeReceived') {
          const isNewDevice = handleDeviceNotice(currentUser.rootId ?? '', event.data);
          if (isNewDevice) {
            notifyDeviceJoined(spaceKeyOf(currentSpace.value), event.data.deviceName);
          }
          return;
        }
        if (event.kind === 'SelfProfileSynced') {
          void loadCurrentUser().then(() => {
            // 同步扩展字段（性别/地区/签名）：loadCurrentUser 只刷新昵称/头像单例，
            // 扩展字段缓存在 profile-extra，需强制重新水合拉取内核最新值
            if (currentUser.rootId) {
              refreshProfileExtraFromKernel(currentUser.rootId);
            }
          });
          return;
        }
        // 组织域数据经自设备 pdsync 合入（org:meta 组织记录 + ct:org 成员附加资料）：
        // 刷新组织列表缓存（name/logo/成员），并对当前 org 空间的我的身份扩展字段
        // 强制重新水合（org 作用域键 rootId@orgId）。
        if (event.kind === 'OrgSynced') {
          void refreshOrganizations().catch(() => {}).then(() => {
            // 成员身份字段（组织内昵称/头像/开关）经内核同步：重水合本地缓存
            refreshOrgIdentity();
            if (currentUser.rootId && currentSpace.value.type === 'org') {
              refreshProfileExtraFromKernel(`${currentUser.rootId}@${currentSpace.value.orgId}`);
            }
          });
          return;
        }
        // M5 延迟恢复：他机发起/否决/提交时刷新本地状态
        if (event.kind === 'RecoveryUpdated') {
          handleRecoveryP2pEvent(currentUser.rootId ?? '', event.data);
          return;
        }
        // 乙+校验器：密码改密/重置/统一完成/水位外事件
        if (
          event.kind === 'PasswordChangeObserved' ||
          event.kind === 'PasswordUnificationDone' ||
          event.kind === 'DeviceOutOfGrace'
        ) {
          handlePasswordUnifyEvent(currentUser.rootId ?? '', event);
          return;
        }
      }).then((un) => unlistenP2p.push(un)).catch(() => {});
    });

    // 主程序更新：后台自动检查+下载就绪后弹重启确认（取消后可去 设置→关于 手动安装）
    useUpdaterReadyPrompt();

    // A7 数据目录迁移失败提示（identity.md §4.3「移动失败回退旧布局并提示」）：
    // 壳层已回退旧布局继续使用，此处一次性告知原因，不阻断。
    // 仅 Tauri 运行时挂监听（测试环境无 __TAURI_INTERNALS__）。
    if (isTauri()) {
      void listenTauriEvent<string>('layout-migration-failed', (e) => {
        ElMessageBox.alert(e.payload, '数据目录迁移未完成', {
          confirmButtonText: '知道了',
          type: 'warning'
        });
      });
    }

    // Android 系统返回键（Android 前端改造）。原生层语义（tauri AppPlugin）：
    // 存在 JS 监听时按返回键只发事件、不执行任何默认动作——handler 返回值无意义，
    // 「退出应用」必须由前端显式调用 system_exit_app（plugin:app|exit 不在 ACL 命令清单内，
    // 前端不可调用）；MainActivity 已禁用 WryActivity 的 WebView 历史回退
    // （handleBackNavigation=false），返回键完全由本 handler 按前端导航栈语义处理。
    onBackButtonPress(() => {
      if (!isMobileLayout.value) {
        return;
      }
      // 1) 覆盖层优先：详情整页覆盖层（我的资料字段详情 / 设置面板内容页）打开时，
      //    先关栈顶覆盖层（逐层回退），而非直接 pop 底层导航栈帧（修复"跳两层"）
      if (hasOverlay()) {
        requestCloseOverlay();
        return;
      }
      // 2) 当前 tab 栈深>1（二级页）：pop 回上一页（不再直接退出应用）
      const tab = activeTab.value;
      if (currentPage(tab).page !== 'root') {
        popPage(tab);
        return;
      }
      // 3) 非主 tab 根页（设置/测试/插件）：与页面返回按钮一致——回来源页而非退出应用
      if (!MAIN_TAB_IDS.includes(tab)) {
        if (tab.startsWith('plugin|')) {
          goBackFromPlugin();
        } else {
          backFromSecondaryTab(tab);
        }
        return;
      }
      // 4) 一级页（主 tab 栈底）：退出应用（原生默认动作已被 JS 监听拦截，须显式退出；
      //    走 system 域收敛点，组件不直接 invoke）
      window.electronAPI?.system.exitApp().catch(() => {});
    }).catch(() => {
      // 桌面端 app 插件无 register_listener 命令，注册静默失败（桌面本无系统返回键事件）
    });

    onMounted(() => window.addEventListener('spark:open-chat', onOpenChatEvent));
    onMounted(() => window.addEventListener('spark:open-app', onOpenAppEvent));
    // 空间二级菜单点击请求切页（RailSpaceList 派发）
    const onSwitchTabEvent = (event: Event) => {
      const tab = (event as CustomEvent<string>).detail;
      if (typeof tab === 'string') {
        handleMenuSelect(tab);
      }
    };
    onMounted(() => window.addEventListener('spark:switch-tab', onSwitchTabEvent));
    const onDesktopShortcut = (event: KeyboardEvent) => {
      if (isMobileLayout.value || !(event.ctrlKey || event.metaKey) || event.altKey) return;
      // ⌘/Ctrl+1~4 固定映射四个一级入口（与 rail 顶部项解耦，「我的」走头像）
      const SHORTCUT_TABS = ['mine', 'messages', 'space', 'affairs'];
      const index = Number(event.key) - 1;
      if (Number.isInteger(index) && index >= 0 && index < SHORTCUT_TABS.length) {
        event.preventDefault();
        handleMenuSelect(SHORTCUT_TABS[index]);
      }
    };
    onMounted(() => window.addEventListener('keydown', onDesktopShortcut));

    onMounted(() => window.addEventListener('spark:open-mine', onOpenMineEvent));
    onMounted(() => window.addEventListener(OPEN_PLUGIN_DEEPLINK_EVENT, onOpenPluginDeepLink));
    // 事务角标数据源：进入主界面拉一次「与我相关」列表（affair-feed 水合，G7 角标随之亮起）
    onMounted(() => {
      void refreshAffairFeed();
    });
    onMounted(() => window.addEventListener('spark:close-plugin', onClosePluginEvent));
    // 网络变化重连（A 秒级感知）：window online/offline 事件 → 通知内核
    // p2p-network-changed（command-map 已有映射，前端调用点缺失）。内核启动
    // 3-5s debounce 后若监听地址确已变化则重发布地址 + 重建 relay + 重拨优先
    // 类目 peer。online 与 offline 都上报——两者都是网络接口变化的可靠信号。
    const onNetworkStateChange = () => {
      window.electronAPI?.p2p.networkChanged().catch(() => {});
    };
    const onNetOnline = () => onNetworkStateChange();
    const onNetOffline = () => onNetworkStateChange();
    onMounted(() => {
      window.addEventListener('online', onNetOnline);
      window.addEventListener('offline', onNetOffline);
    });
    onUnmounted(() => {
      window.removeEventListener('keydown', onDesktopShortcut);
      window.removeEventListener('spark:switch-tab', onSwitchTabEvent);
      window.removeEventListener('spark:open-chat', onOpenChatEvent);
      window.removeEventListener('spark:open-app', onOpenAppEvent);
      window.removeEventListener('spark:open-mine', onOpenMineEvent);
      window.removeEventListener(OPEN_PLUGIN_DEEPLINK_EVENT, onOpenPluginDeepLink);
      window.removeEventListener('spark:close-plugin', onClosePluginEvent);
      window.removeEventListener('online', onNetOnline);
      window.removeEventListener('offline', onNetOffline);
      unlistenP2p.forEach((un) => un());
    });

    return {
      desktopVisited,
      openDesktopSearch,
      railSpaceListRef,
      onSpaceMembershipCommand,
      isDevelopment,
      onRailMoreCommand,
      activeTab,
      pluginTabs,
      railExpanded,
      isMobileLayout,
      toggleRail,
      navItems,
      isNavActive,
      messagesBadge,
      affairsBadge,
      activePluginTab,
      pluginSpace,
      pluginManifest,
      pluginHostTitleBar,
      onPluginManifest,
      closePluginTab,
      handleMenuSelect,
      backFromSecondaryTab,
      openPluginTab,
      goBackFromPlugin,
      onBuiltinPluginClose,
      loadCurrentUser,
      handleSwitchAccount,
      handleLogout,
      mobileDrawerVisible,
      mobileTopBarVisible,
      mobileTabBarVisible,
      mobileTopBarTitle,
      openNetworkStatus,
      onMobileAddContact
    };
  }
});
</script>
