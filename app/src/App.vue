<template>
  <div class="shell">
    <!-- 顶部导航（最外层，横跨全宽）：
         桌面端=TopNavbar（左侧空间切换+中间搜索+右侧网络状态+「⋯」菜单）；
         移动端=MobileTopBar，仅四个主 tab 且栈深=1（一级列表页）时显示，
         切到二级页/其它 tab 时直接不渲染（无位移动画，Android 前端改造） -->
    <header class="topbar" :class="{ 'topbar-collapsed': isMobileLayout && !mobileTopBarVisible }">
      <TopNavbar
        v-if="!isMobileLayout"
        @switch-account="handleSwitchAccount"
        @logout="handleLogout"
        @open-tab="handleMenuSelect"
      />
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
        <div class="rail-identity">
          <UserAvatarMenu
            :avatar-size="36"
            @open-profile="handleMenuSelect('mine')"
          />
        </div>

        <div class="rail-main">
          <button
            v-for="item in navItems"
            :key="item.id"
            class="rail-item"
            :class="{ active: activeTab === item.id }"
            @click="handleMenuSelect(item.id)"
          >
            <!-- 消息入口挂当前空间未读总数角标（免打扰不计入，>99 显示 99+），品牌红 -->
            <el-badge v-if="item.id === 'messages'" :value="messagesBadge" :max="99" :hidden="messagesBadge === 0">
              <el-icon :size="20"><component :is="item.icon" /></el-icon>
            </el-badge>
            <!-- 通讯录入口挂「新的朋友/成员」未读角标（当前空间） -->
            <el-badge
              v-else-if="item.id === 'contacts'"
              :value="contactsBadge"
              :max="99"
              :hidden="contactsBadge === 0"
            >
              <el-icon :size="20"><component :is="item.icon" /></el-icon>
            </el-badge>
            <el-icon v-else :size="20"><component :is="item.icon" /></el-icon>
            <span class="rail-label">{{ item.label }}</span>
          </button>

          <button
            v-for="tab in pluginTabs"
            :key="tab.id"
            class="rail-item rail-plugin"
            :class="{ active: activeTab === tab.id }"
            :title="tab.title"
            @click="handleMenuSelect(tab.id)"
          >
            <span class="rail-plugin-icon">{{ tab.icon }}</span>
            <span class="rail-label">{{ tab.title }}</span>
          </button>
        </div>

        <div class="rail-divider" aria-hidden="true" />
        <div class="rail-bottom">
          <!-- TODO: 测试入口仅开发/调试构建显示，正式发版通过构建配置隐藏（ui-space-navbar §6.4） -->
          <button
            class="rail-item"
            :class="{ active: activeTab === 'test' }"
            @click="handleMenuSelect('test')"
          >
            <el-icon :size="20"><Cpu /></el-icon>
            <span class="rail-label">测试</span>
          </button>
          <button
            class="rail-item"
            :class="{ active: activeTab === 'settings' }"
            @click="handleMenuSelect('settings')"
          >
            <el-icon :size="20"><Setting /></el-icon>
            <span class="rail-label">设置</span>
          </button>
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
            <MessagesPage v-if="activeTab === 'messages'" />
            <ContactsPage v-else-if="activeTab === 'contacts'" />
            <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
            <TestPage v-else-if="activeTab === 'test'" @back-root="backFromSecondaryTab('test')" />
            <SettingsPage
              v-else-if="activeTab === 'settings'"
              @profile-updated="loadCurrentUser"
              @open-tab="handleMenuSelect"
              @back-root="backFromSecondaryTab('settings')"
            />
            <!-- 「我的资料」隐藏入口：点击 rail 顶部头像进入，不在 rail 展示 -->
            <MinePage v-else-if="activeTab === 'mine'" @profile-updated="loadCurrentUser" />

            <el-card v-else-if="activePluginTab" shadow="never" class="plugin-tab-card">
              <template #header>
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
                @close="closePluginTab"
              />
            </el-card>
          </div>
        </Transition>

        <!-- 桌面端（≥769px）：渲染逻辑不变，无任何切换动画 -->
        <template v-else>
          <MessagesPage v-if="activeTab === 'messages'" />
          <ContactsPage v-else-if="activeTab === 'contacts'" />
          <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
          <TestPage v-else-if="activeTab === 'test'" @back-root="backFromSecondaryTab('test')" />
          <SettingsPage
            v-else-if="activeTab === 'settings'"
            @profile-updated="loadCurrentUser"
            @open-tab="handleMenuSelect"
            @back-root="backFromSecondaryTab('settings')"
          />
          <!-- 「我的资料」隐藏入口：点击 rail 顶部头像进入，不在 rail 展示 -->
          <MinePage v-else-if="activeTab === 'mine'" @profile-updated="loadCurrentUser" />

          <el-card v-else-if="activePluginTab" shadow="never" class="plugin-tab-card">
            <template #header>
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
              @close="closePluginTab"
            />
          </el-card>
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
      :contacts-badge="contactsBadge"
      @select="handleMenuSelect"
    />

    <!-- 移动端左滑侧边栏（Android 前端改造）：空间切换 + 加入/创建 + 设置；「⋯」菜单已下放此处与设置页 -->
    <MobileSpaceDrawer
      v-if="isMobileLayout"
      v-model="mobileDrawerVisible"
      @open-settings="handleMenuSelect('settings')"
    />

    <!-- 后台常驻插件视图（隐藏 iframe，承载 bot 消息监听等常驻任务） -->
    <PluginBackgroundHost
      v-for="bg in backgroundPlugins"
      :key="bg.pluginId"
      :plugin-id="bg.pluginId"
      :view-id="bg.viewId"
      :space="pluginSpace"
    />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { onBackButtonPress } from '@tauri-apps/api/app';
import { ChatDotRound, Cpu, Grid, Notebook, Setting } from '@element-plus/icons-vue';
import type { PluginSpaceContext } from '../../packages/plugin-sdk/src';
import PluginIframeHost from './components/plugin/PluginIframeHost.vue';
import PluginBackgroundHost from './components/plugin/PluginBackgroundHost.vue';
import { fetchPluginManifest } from './plugin/source';
import { unreadCountOf } from './stores/messages';
import { requestBadgeCount, spaceKeyOf } from './mock/contacts';
import {
  currentSpace,
  validateCurrentSpace
} from './stores/current-space';
import { refreshCurrentUser } from './stores/current-user';
import { requestOpenChat } from './stores/pending-chat';
import { requestOpenContact } from './stores/pending-contact';
import { requestAddContact, type AddContactKind } from './stores/pending-add-contact';
import { requestOpenAppDetail } from './stores/pending-app';
import TopNavbar from './components/TopNavbar.vue';
import UserAvatarMenu from './components/UserAvatarMenu.vue';
import MobileTabBar from './components/MobileTabBar.vue';
import MobileTopBar from './components/MobileTopBar.vue';
import MobileSpaceDrawer from './components/MobileSpaceDrawer.vue';
import { isMobileLayout, MOBILE_TABS } from './stores/ui-layout';
import { invoke } from '@tauri-apps/api/core';
import { listenP2pEvents } from './api';
import { currentPage, popPage, resetStack } from './stores/mobile-nav';
import { hasOverlay, requestCloseOverlay } from './stores/overlay-stack';
import { requestOpenSystemSection } from './stores/pending-system-section';
import { useUpdaterReadyPrompt } from './components/updater/use-updater';
import MessagesPage from './pages/MessagesPage.vue';
import ContactsPage from './pages/ContactsPage.vue';
import AppsPage, { type OpenPluginTabPayload } from './pages/AppsPage.vue';
import TestPage from './pages/TestPage.vue';
import SettingsPage from './pages/SettingsPage.vue';
import MinePage from './pages/MinePage.vue';

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
};

export default defineComponent({
  name: 'App',
  components: {
    MessagesPage,
    ContactsPage,
    AppsPage,
    TestPage,
    PluginBackgroundHost,
    SettingsPage,
    MinePage,
    TopNavbar,
    UserAvatarMenu,
    MobileTabBar,
    MobileTopBar,
    MobileSpaceDrawer,
    PluginIframeHost,
    ChatDotRound,
    Notebook,
    Grid,
    Cpu,
    Setting
  },
  setup() {
    const activeTab = ref<string>('messages');
    // 移动端左滑侧边栏可见性（Android 前端改造）
    const mobileDrawerVisible = ref(false);
    const pluginTabs = ref<PluginTab[]>([]);
    // rail 宽窄状态（持久化）：false=64px 窄栏（图标+小字），true=155px 宽栏（左图标右文字）
    const railExpanded = ref(localStorage.getItem('spark:rail-expanded') === '1');
    const toggleRail = () => {
      railExpanded.value = !railExpanded.value;
      localStorage.setItem('spark:rail-expanded', railExpanded.value ? '1' : '0');
    };
    // 当前登录用户资料：stores/current-user 单例（rail 头像/空间切换器/消息气泡自己头像共用）；
    // 主窗口挂载时读取一次，资料更新（profile-updated）后重新读取
    const loadCurrentUser = refreshCurrentUser;

    // rail 主导航：消息固定为第一个入口（ui-space-navbar §14）
    const navItems = [
      { id: 'messages', label: '消息', icon: ChatDotRound },
      { id: 'contacts', label: '通讯录', icon: Notebook },
      { id: 'apps', label: '应用', icon: Grid }
    ];

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

    /** 移动端顶栏「+」菜单：先写添加请求（pending-add-contact，同 pending-chat 模式），
        再切到通讯录 tab——ContactsPage 挂载/监听后消费并打开对应添加对话框 */
    const onMobileAddContact = (kind: AddContactKind) => {
      requestAddContact(kind);
      handleMenuSelect('contacts');
    };

    /** 通讯录入口角标：当前空间「新的朋友/成员」未读条目数 */
    const contactsBadge = computed(() => requestBadgeCount(spaceKeyOf(currentSpace.value)));

    /** 消息入口角标：当前空间未读消息总数（与通讯录角标一样按空间隔离） */
    const messagesBadge = computed(() => unreadCountOf(spaceKeyOf(currentSpace.value)));

    const activePluginTab = computed(() => {
      return pluginTabs.value.find((tab) => tab.id === activeTab.value) ?? null;
    });

    /** 插件运行 space 上下文（透传 PluginIframeHost；个人空间 id 恒 'personal'） */
    const pluginSpace = computed<PluginSpaceContext>(() => ({
      type: currentSpace.value.type,
      id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    }));

    /** 后台常驻视图列表：声明了 type:'background' 视图的已启用插件，应用启动即挂载，
        隐藏 iframe 常驻运行（承载 bot 消息监听等常驻任务，不依赖可见视图） */
    const backgroundPlugins = ref<Array<{ pluginId: string; viewId: string }>>([]);
    const loadBackgroundPlugins = async (): Promise<void> => {
      try {
        const items = await window.electronAPI?.pluginMarket.list();
        if (!items) return;
        const result: Array<{ pluginId: string; viewId: string }> = [];
        for (const item of items) {
          if (!item.installed || !item.enabled) continue;
          try {
            const manifest = await fetchPluginManifest(item.id);
            const bgView = manifest?.views?.find((v) => v.type === 'background');
            if (bgView) result.push({ pluginId: item.id, viewId: bgView.id });
          } catch {
            /* 单插件清单不可读则跳过，不阻塞其余 */
          }
        }
        backgroundPlugins.value = result;
      } catch {
        /* 插件市场不可用（非 Tauri 环境等）：无后台宿主 */
      }
    };

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

    const openPluginTab = (payload: OpenPluginTabPayload) => {
      const pluginDomain = payload.pluginDomain.trim();
      const pluginView = payload.pluginView.trim() || 'default';
      if (!pluginDomain.startsWith('plugin:')) {
        ElMessage.error(`无效插件域：${pluginDomain}`);
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
          pluginContext
        });
      }
      activeTab.value = tabId;
    };

    const goBackFromPlugin = () => {
      const tab = activePluginTab.value;
      const fallback = 'apps';
      activeTab.value = tab?.sourceTab ?? fallback;
    };

    // 锁定身份后整窗重载回登录/选择账号页（RootGate 接管），与 RootGate.handleLogout 同一语义
    const lockAndReload = async (successText: string) => {
      try {
        await window.electronAPI.rootIdentity.lock();
        ElMessage.success(successText);
        window.location.reload();
      } catch (error) {
        ElMessage.error(`操作失败：${error}`);
      }
    };

    const handleLogout = () => lockAndReload('已退出登录');
    const handleSwitchAccount = () => lockAndReload('已退出当前账号');

    // 通讯录/应用市场请求打开 1:1 会话（ui-contacts §5.3）：记录请求并切到消息页
    const onOpenChatEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ rootId?: string; name?: string; conversationId?: string }>).detail;
      if (!detail?.rootId) {
        return;
      }
      requestOpenChat({ rootId: detail.rootId, name: detail.name ?? '', conversationId: detail.conversationId });
      activeTab.value = 'messages';
    };

    // 全局搜索请求打开联系人资料：记录请求并切到通讯录页（ContactsPage 消费）
    const onOpenContactEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ rootId?: string }>).detail;
      if (!detail?.rootId) {
        return;
      }
      requestOpenContact({ rootId: detail.rootId });
      activeTab.value = 'contacts';
    };

    // 全局搜索请求打开应用详情：记录请求并切到应用页（AppsPage 消费）
    const onOpenAppEvent = (event: Event) => {
      const detail = (event as CustomEvent<{ id?: string }>).detail;
      if (!detail?.id) {
        return;
      }
      requestOpenAppDetail(detail.id);
      activeTab.value = 'apps';
    };

    // 设置页「个人资料」入口卡请求打开个人设置：直接切到 mine tab（参照 spark:open-chat 模式）
    const onOpenMineEvent = () => {
      activeTab.value = 'mine';
    };

    // p2p 事件退订器收集（SelfProfileSynced 等；卸载时统一退订）
    const unlistenP2p: Array<() => void> = [];

    onMounted(() => {
      void loadCurrentUser();
      // 懒校验启动恢复的组织空间：组织已不存在时回退个人空间
      void validateCurrentSpace();
      // 挂载后台常驻插件视图（bot 消息监听等），不依赖可见视图
      void loadBackgroundPlugins();
      // 插件后台运行时对账（内核 QuickJS 沙箱）：身份切换会停全部插件后台，
      // 进入主界面按当前身份重新拉起（幂等；旧 background iframe 视图下线后
      // 此调用成为唯一入口）
      window.electronAPI?.pluginRuntime?.syncBackgrounds().catch(() => {});
      // 自设备资料同步（多设备）：本机资料被其他设备的全量快照更新后刷新展示
      void listenP2pEvents((event) => {
        if (event.kind === 'SelfProfileSynced') {
          void loadCurrentUser();
        }
      }).then((un) => unlistenP2p.push(un)).catch(() => {});
    });

    // 主程序更新：后台自动检查+下载就绪后弹重启确认（取消后可去 设置→关于 手动安装）
    useUpdaterReadyPrompt();

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
      // 4) 一级页（主 tab 栈底）：退出应用（原生默认动作已被 JS 监听拦截，须显式退出）
      invoke('system_exit_app').catch(() => {});
    }).catch(() => {
      // 桌面端 app 插件无 register_listener 命令，注册静默失败（桌面本无系统返回键事件）
    });

    onMounted(() => window.addEventListener('spark:open-chat', onOpenChatEvent));
    onMounted(() => window.addEventListener('spark:open-contact', onOpenContactEvent));
    onMounted(() => window.addEventListener('spark:open-app', onOpenAppEvent));
    onMounted(() => window.addEventListener('spark:open-mine', onOpenMineEvent));
    onMounted(() => window.addEventListener('spark:close-plugin', onClosePluginEvent));
    onUnmounted(() => {
      window.removeEventListener('spark:open-chat', onOpenChatEvent);
      window.removeEventListener('spark:open-contact', onOpenContactEvent);
      window.removeEventListener('spark:open-app', onOpenAppEvent);
      window.removeEventListener('spark:open-mine', onOpenMineEvent);
      window.removeEventListener('spark:close-plugin', onClosePluginEvent);
      unlistenP2p.forEach((un) => un());
    });

    return {
      activeTab,
      pluginTabs,
      railExpanded,
      isMobileLayout,
      toggleRail,
      navItems,
      messagesBadge,
      contactsBadge,
      activePluginTab,
      pluginSpace,
      backgroundPlugins,
      closePluginTab,
      handleMenuSelect,
      backFromSecondaryTab,
      openPluginTab,
      goBackFromPlugin,
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
