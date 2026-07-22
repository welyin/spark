<template>
  <section v-if="isPluginViewMode" class="plugin-host-wrap">
    <el-card shadow="never">
      <el-alert v-if="pluginHostMessage" :title="pluginHostMessage" type="warning" :closable="false" show-icon />
      <component v-if="activePluginView" :is="activePluginView" />
    </el-card>
  </section>

  <div v-else class="shell">
    <nav class="rail">
      <button
        class="rail-avatar"
        :class="{ active: activeTab === 'mine' }"
        title="我的"
        @click="handleMenuSelect('mine')"
      >
        <UserAvatar :root-id="currentUser.rootId ?? ''" :nickname="currentUser.nickname" :avatar="currentUser.avatar" :size="38" />
      </button>

      <div class="rail-main">
        <button
          v-for="item in navItems"
          :key="item.id"
          class="rail-item"
          :class="{ active: activeTab === item.id }"
          @click="handleMenuSelect(item.id)"
        >
          <el-icon :size="20"><component :is="item.icon" /></el-icon>
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

      <div class="rail-bottom">
        <button
          class="rail-item"
          :class="{ active: activeTab === 'test' }"
          @click="handleMenuSelect('test')"
        >
          <el-icon :size="20"><Tools /></el-icon>
          <span class="rail-label">测试</span>
        </button>
      </div>
    </nav>

    <main class="main">
      <AffairsPage v-if="activeTab === 'affairs'" />
      <OrgPage v-else-if="activeTab === 'org'" @open-plugin-tab="openPluginTab" />
      <AppsPage v-else-if="activeTab === 'apps'" @open-plugin-tab="openPluginTab" />
      <TestPage v-else-if="activeTab === 'test'" />
      <MinePage v-else-if="activeTab === 'mine'" />

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
        <iframe
          class="plugin-frame"
          :src="pluginFrameSrc"
          :title="`${activePluginTab.pluginDomain}/${activePluginTab.pluginView}`"
        />
      </el-card>
    </main>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, shallowRef, type Component } from 'vue';
import { Bell, Grid, OfficeBuilding, Tools } from '@element-plus/icons-vue';
import { getPluginView } from './plugin-view-registry';
import UserAvatar from './components/UserAvatar.vue';
import AffairsPage from './pages/AffairsPage.vue';
import OrgPage from './pages/OrgPage.vue';
import AppsPage, { type OpenPluginTabPayload } from './pages/AppsPage.vue';
import TestPage from './pages/TestPage.vue';
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
    AffairsPage,
    OrgPage,
    AppsPage,
    TestPage,
    MinePage,
    UserAvatar,
    Bell,
    Grid,
    OfficeBuilding,
    Tools
  },
  setup() {
    const search = new URLSearchParams(window.location.search);
    const pluginWindowDomain = search.get('pluginDomain');
    const pluginWindowView = search.get('pluginView') ?? 'default';
    const isPluginViewMode = ref(Boolean(pluginWindowDomain));
    const activePluginView = shallowRef<Component | null>(null);
    const pluginHostMessage = ref('');

    const activeTab = ref<string>('apps');
    const pluginTabs = ref<PluginTab[]>([]);
    // 当前登录用户资料（导航栏头像）；主窗口挂载时读取一次，切换用户会重建本组件
    const currentUser = ref<{ rootId: string | null; nickname: string; avatar: string }>({
      rootId: null,
      nickname: '',
      avatar: ''
    });

    // 深色导航栏内置入口（我的=顶部头像；测试=底部）
    const navItems = [
      { id: 'affairs', label: '事务', icon: Bell },
      { id: 'org', label: '组织', icon: OfficeBuilding },
      { id: 'apps', label: '应用', icon: Grid }
    ];

    const activePluginTab = computed(() => {
      return pluginTabs.value.find((tab) => tab.id === activeTab.value) ?? null;
    });

    const pluginFrameSrc = computed(() => {
      const tab = activePluginTab.value;
      if (!tab) {
        return '';
      }

      const url = new URL(window.location.href);
      url.search = '';
      url.searchParams.set('pluginDomain', tab.pluginDomain);
      url.searchParams.set('pluginView', tab.pluginView);
      if (tab.pluginContext?.orgId) {
        url.searchParams.set('orgId', tab.pluginContext.orgId);
      }
      return url.toString();
    });

    const handleMenuSelect = (index: string) => {
      activeTab.value = index;
    };

    const openPluginTab = (payload: OpenPluginTabPayload) => {
      const pluginDomain = payload.pluginDomain.trim();
      const pluginView = payload.pluginView.trim() || 'default';
      if (!pluginDomain.startsWith('plugin:')) {
        resultMessage.value = `无效插件域：${pluginDomain}`;
        return;
      }

      const pluginContext = payload.pluginContext;
      const contextSuffix = pluginContext?.orgId ? `|${pluginContext.orgId}` : '';

      const tabId = `plugin|${pluginDomain}|${pluginView}${contextSuffix}`;
      const existing = pluginTabs.value.find((item) => item.id === tabId);
      if (!existing) {
        const sourceTab = activeTab.value.startsWith('plugin|') ? 'org' : activeTab.value;
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
      const fallback = 'org';
      activeTab.value = tab?.sourceTab ?? fallback;
    };

    onMounted(() => {
      if (isPluginViewMode.value && pluginWindowDomain) {
        const view = getPluginView(pluginWindowDomain, pluginWindowView);
        if (view) {
          activePluginView.value = view;
        } else {
          pluginHostMessage.value = `未找到插件视图：${pluginWindowDomain} / ${pluginWindowView}`;
        }
        return;
      }

      window.electronAPI.rootIdentity
        .status()
        .then((status) => {
          currentUser.value = {
            rootId: status.rootId,
            nickname: status.nickname ?? '',
            avatar: status.avatar ?? ''
          };
        })
        .catch(() => {
          // 读取失败时保留默认自动头像
        });
    });

    return {
      isPluginViewMode,
      pluginWindowDomain,
      pluginWindowView,
      activePluginView,
      pluginHostMessage,
      activeTab,
      pluginTabs,
      navItems,
      currentUser,
      activePluginTab,
      pluginFrameSrc,
      handleMenuSelect,
      openPluginTab,
      goBackFromPlugin
    };
  }
});
</script>
