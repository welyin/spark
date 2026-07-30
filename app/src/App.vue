<template>
  <div class="shell">
    <!-- 顶部导航栏（最外层，横跨全宽）：左侧=空间切换+网络状态，右侧=「⋯」更多（切换账号/退出登录） -->
    <header class="topbar">
      <TopNavbar
        @switch-account="handleSwitchAccount"
        @logout="handleLogout"
      />
    </header>

    <div class="shell-body">
      <!-- rail：顶部=当前身份头像（点击直达「我的资料」），中部=当前空间内的二级导航（ui-space-navbar §5）。
           两种状态：窄栏（64px，图标在上文字在下）/ 宽栏（155px，左图标右文字），
           点右侧分隔线（col-resize 光标）切换，选择持久化在 localStorage -->
      <nav class="rail" :class="{ expanded: railExpanded }">
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
        <MessagesPage v-if="activeTab === 'messages'" />
        <ContactsPage v-else-if="activeTab === 'contacts'" />
        <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
        <TestPage v-else-if="activeTab === 'test'" />
        <SettingsPage
          v-else-if="activeTab === 'settings'"
          @profile-updated="loadCurrentUser"
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
      </main>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { ChatDotRound, Cpu, Grid, Notebook, Setting } from '@element-plus/icons-vue';
import type { PluginSpaceContext } from '../../packages/plugin-sdk/src';
import PluginIframeHost from './components/plugin/PluginIframeHost.vue';
import { unreadCountOf } from './mock/messages';
import { requestBadgeCount, spaceKeyOf } from './mock/contacts';
import {
  currentSpace,
  validateCurrentSpace
} from './stores/current-space';
import { refreshCurrentUser } from './stores/current-user';
import { requestOpenChat } from './stores/pending-chat';
import { requestOpenContact } from './stores/pending-contact';
import { requestOpenAppDetail } from './stores/pending-app';
import TopNavbar from './components/TopNavbar.vue';
import UserAvatarMenu from './components/UserAvatarMenu.vue';
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
    SettingsPage,
    MinePage,
    TopNavbar,
    UserAvatarMenu,
    PluginIframeHost,
    ChatDotRound,
    Notebook,
    Grid,
    Cpu,
    Setting
  },
  setup() {
    const activeTab = ref<string>('messages');
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

    /** 关闭当前插件 tab（熔断覆盖层「关闭」；移除 tab 并回来源页） */
    const closePluginTab = () => {
      const tab = activePluginTab.value;
      if (!tab) {
        return;
      }
      pluginTabs.value = pluginTabs.value.filter((item) => item.id !== tab.id);
      activeTab.value = tab.sourceTab ?? 'apps';
    };

    const handleMenuSelect = (index: string) => {
      activeTab.value = index;
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

    onMounted(() => {
      void loadCurrentUser();
      // 懒校验启动恢复的组织空间：组织已不存在时回退个人空间
      void validateCurrentSpace();
    });

    onMounted(() => window.addEventListener('spark:open-chat', onOpenChatEvent));
    onMounted(() => window.addEventListener('spark:open-contact', onOpenContactEvent));
    onMounted(() => window.addEventListener('spark:open-app', onOpenAppEvent));
    onMounted(() => window.addEventListener('spark:open-mine', onOpenMineEvent));
    onUnmounted(() => {
      window.removeEventListener('spark:open-chat', onOpenChatEvent);
      window.removeEventListener('spark:open-contact', onOpenContactEvent);
      window.removeEventListener('spark:open-app', onOpenAppEvent);
      window.removeEventListener('spark:open-mine', onOpenMineEvent);
    });

    return {
      activeTab,
      pluginTabs,
      railExpanded,
      toggleRail,
      navItems,
      messagesBadge,
      contactsBadge,
      activePluginTab,
      pluginSpace,
      closePluginTab,
      handleMenuSelect,
      openPluginTab,
      goBackFromPlugin,
      loadCurrentUser,
      handleSwitchAccount,
      handleLogout
    };
  }
});
</script>
