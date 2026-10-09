<!-- 组织管理插件根组件（A42，移植自壳层 components/org 组织设置面板族）：
     - org 空间：当前空间组织的管理界面（第三栏子菜单 + 第四栏内容，
       与壳层 OrgSettingsPanel 同构；移动端子菜单整页 ⇄ 内容整页覆盖）；
     - personal 空间：创建/加入 + 如实提示（A42 评审决议：sdk.org.listMine
       与老面 runtime.listMineOrganizations 口径统一为 personal 拒绝——
       personal 空间无组织可管，插件不调 listMine，提示用户切换空间）。
     数据面全部走 sdk.org / sdk.policy（桥）。壳层 localStorage 面
     （org-avatars 展示缓存）与空间切换动作不迁移——见任务报告迁移对照表。 -->
<template>
  <section class="org-admin-root">
    <el-alert v-if="loadError" class="org-load-error" :title="loadError" type="error" :closable="false" show-icon />

    <!-- ── org 空间：管理界面 ── -->
    <template v-if="isOrgSpace">
      <!-- 移动端内容整页返回栏：选中 section 时显示，返回回子菜单 -->
      <div v-if="mobileBarVisible" class="org-mobile-bar">
        <el-button text type="primary" @click="activeSection = null">&lt; {{ activeSectionLabel }}</el-button>
      </div>

      <!-- 第三栏：子菜单（移动端选中 section 时隐藏，内容整页覆盖） -->
      <div v-show="activeSection === null || !isNarrow" class="org-submenu">
        <h2 class="org-submenu-title">组织管理</h2>
        <button
          v-for="item in sections"
          :key="item.key"
          type="button"
          class="org-submenu-item"
          :class="{ active: activeSection === item.key }"
          @click="activeSection = item.key"
        >{{ item.label }}</button>
      </div>

      <!-- 第四栏：当前子项内容 -->
      <div v-show="activeSection !== null || !isNarrow" class="org-detail">
        <el-empty v-if="loading" description="正在加载组织信息..." />
        <el-empty v-else-if="!organization && activeSection !== 'membership'" description="当前空间组织信息加载失败，请切换空间后重试。" />

        <template v-else>
          <OrgInfoPanel
            v-if="activeSection === 'info' && organization"
            :org="organization"
            :overview="overview"
            :gateway-active-set="gatewayActiveSet"
            @changed="reload"
          />
          <OrgRosterPanel
            v-else-if="activeSection === 'roster' && organization"
            :key="`roster-${organization.updatedAt}`"
            :org="organization"
            @changed="reload"
          />
          <OrgPolicyPanel
            v-else-if="activeSection === 'policy' && organization"
            :org-id="organization.orgId"
            :is-admin="organization.isCurrentUserAdmin"
          />
          <OrgPublicPanel
            v-else-if="activeSection === 'public' && organization"
            :org="organization"
            @changed="reload"
          />
          <OrgDiscoverPanel v-else-if="activeSection === 'discover'" />
          <OrgRecoverPanel v-else-if="activeSection === 'recover' && organization" :org-id="organization.orgId" />
          <template v-else-if="activeSection === 'purge' && organization">
            <el-card shadow="never" class="panel-card">
              <template #header>
                <h2>数据治理</h2>
              </template>
              <OrgPurgePanel v-if="organization.isCurrentUserAdmin" :org-id="organization.orgId" />
              <p v-else class="hint">仅管理员可进行数据治理。</p>
            </el-card>
          </template>
          <OrgCreateJoinPanel v-else-if="activeSection === 'membership'" @changed="reload" />
        </template>
      </div>
    </template>

    <!-- ── personal 空间：创建/加入 + 如实提示（listMine personal 拒绝，A42 评审决议） ── -->
    <template v-else>
      <div class="org-personal">
        <el-card shadow="never" class="panel-card">
          <template #header>
            <h2>我的组织</h2>
          </template>
          <el-empty :description="personalNotice" />
        </el-card>
        <OrgCreateJoinPanel @changed="reload" />
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref } from 'vue';
import type { PluginOrgSyncOverview, PluginOrgView } from '../../../packages/plugin-sdk/src';
import { orgApi, pluginContext } from './sdk-host';
import { loadSpaceOrganization, ORG_SECTIONS, orgSectionLabel, PERSONAL_SPACE_NOTICE, type OrgSectionKey } from './org-store';
import OrgInfoPanel from './components/OrgInfoPanel.vue';
import OrgRosterPanel from './components/OrgRosterPanel.vue';
import OrgPolicyPanel from './components/OrgPolicyPanel.vue';
import OrgPublicPanel from './components/OrgPublicPanel.vue';
import OrgDiscoverPanel from './components/OrgDiscoverPanel.vue';
import OrgRecoverPanel from './components/OrgRecoverPanel.vue';
import OrgPurgePanel from './components/OrgPurgePanel.vue';
import OrgCreateJoinPanel from './components/OrgCreateJoinPanel.vue';

/** 窄屏断点（与壳层 ui-layout 的 768px 移动端口径一致；插件内不依赖壳层 store） */
const NARROW_QUERY = '(max-width: 768px)';

export default defineComponent({
  name: 'OrgAdminApp',
  components: {
    OrgInfoPanel,
    OrgRosterPanel,
    OrgPolicyPanel,
    OrgPublicPanel,
    OrgDiscoverPanel,
    OrgRecoverPanel,
    OrgPurgePanel,
    OrgCreateJoinPanel
  },
  setup() {
    const isOrgSpace = computed(() => pluginContext()?.space.type === 'org');
    const sections = ORG_SECTIONS;
    const isNarrow = ref(false);
    const activeSection = ref<OrgSectionKey | null>(null);
    const organization = ref<PluginOrgView | null>(null);
    const overview = ref<PluginOrgSyncOverview | null>(null);
    const gatewayActiveSet = ref<string[]>([]);
    const loading = ref(false);
    const loadError = ref('');
    const personalNotice = PERSONAL_SPACE_NOTICE;

    const activeSectionLabel = computed(() => orgSectionLabel(activeSection.value));
    const mobileBarVisible = computed(() => isNarrow.value && activeSection.value !== null);

    // 窄屏媒体查询（壳层 isMobileLayout store 的插件自足替代）
    const media = window.matchMedia(NARROW_QUERY);
    const applyMedia = () => {
      isNarrow.value = media.matches;
      // 宽屏缺省展开「组织信息」；窄屏回到子菜单（内容整页覆盖由 v-show 承担）
      activeSection.value = media.matches ? null : 'info';
    };
    const onMediaChange = () => applyMedia();

    const reload = async () => {
      const api = orgApi();
      const ctx = pluginContext();
      if (!api || !ctx) {
        return;
      }
      loading.value = true;
      try {
        // personal 空间不调 listMine（桥按空间拒绝，A42 评审决议），返回 null 由模板如实提示
        const found = await loadSpaceOrganization(api, ctx.space);
        organization.value = found;
        if (found) {
          try {
            overview.value = await api.getSyncOverview(found.orgId);
          } catch {
            overview.value = null;
          }
          try {
            gatewayActiveSet.value = await api.getGatewayActiveSet(found.orgId);
          } catch {
            gatewayActiveSet.value = [];
          }
        } else {
          overview.value = null;
          gatewayActiveSet.value = [];
        }
        loadError.value = '';
      } catch (error) {
        loadError.value = `加载组织失败：${error}`;
        organization.value = null;
        overview.value = null;
      } finally {
        loading.value = false;
      }
    };

    onMounted(() => {
      applyMedia();
      media.addEventListener('change', onMediaChange);
      void reload();
    });
    onBeforeUnmount(() => media.removeEventListener('change', onMediaChange));

    return {
      isOrgSpace,
      sections,
      isNarrow,
      activeSection,
      activeSectionLabel,
      mobileBarVisible,
      organization,
      overview,
      gatewayActiveSet,
      personalNotice,
      loading,
      loadError,
      reload
    };
  }
});
</script>

<style>
/* 设计令牌与根样式（插件自足，与 spark-chat tokens.css 同口径；不 scoped） */
@import './styles/tokens.css';
</style>

<style scoped>
.org-admin-root {
  display: flex;
  height: 100%;
  min-height: 0;
  background: var(--spark-bg-card);
}

.org-load-error {
  position: absolute;
  z-index: 5;
  margin: 8px;
}

/* 第三栏：子菜单 */
.org-submenu {
  width: 200px;
  flex-shrink: 0;
  border-right: 1px solid var(--spark-border-light);
  padding: 12px 8px;
  overflow-y: auto;
}

.org-submenu-title {
  margin: 4px 8px 12px;
  font-size: 16px;
}

.org-submenu-item {
  display: block;
  width: 100%;
  border: 0;
  background: transparent;
  text-align: left;
  padding: 9px 12px;
  border-radius: var(--spark-radius-m);
  font-size: 14px;
  color: var(--spark-text-1);
  cursor: pointer;
}

.org-submenu-item:hover {
  background: var(--spark-bg-hover);
}

.org-submenu-item.active {
  background: var(--el-color-primary-light-9);
  color: var(--spark-primary);
  font-weight: 600;
}

/* 第四栏：内容 */
.org-detail {
  flex: 1;
  min-width: 0;
  min-height: 0;
  overflow-y: auto;
  padding: 16px;
}

.panel-card h2 {
  margin: 0;
  font-size: 16px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

/* personal 空间布局 */
.org-personal {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

/* 移动端返回栏（插件自足，替代壳层 MobileBackBar + overlay-stack） */
.org-mobile-bar {
  display: none;
}

@media (max-width: 768px) {
  .org-admin-root {
    flex-direction: column;
  }

  .org-mobile-bar {
    display: block;
    border-bottom: 1px solid var(--spark-border-light);
    padding: 4px 8px;
  }

  .org-submenu {
    width: 100%;
    border-right: 0;
    flex: 1;
  }

  .org-detail {
    padding: 12px;
  }
}
</style>
