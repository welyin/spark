<!-- 组织空间设置页「组织设置」面板：只服务当前空间的一个组织。
     从 OrgPage 的组织列表+详情抽屉裁剪而来（列表/多组织选中/抽屉已移除），
     布局与 SystemSettingsPanel 同构：第三栏子菜单 + 第四栏内容。
     子项：组织信息（字段逐行展示）/ 网关设置 / 找回组织 / 数据治理 / 公开设置 / 发现公开组织 -->
<template>
  <div class="org-settings-panel">
  <!-- 移动端内容整页返回栏（Android 前端改造）：选中 section 时显示，返回回子菜单；
       Transition 进出自右滑入/向右滑出（与导航栈 push/pop 同向） -->
  <Transition name="settings-overlay-slide">
  <div v-if="isMobileLayout && activeSection !== null" class="settings-mobile-bar">
    <MobileBackBar :title="activeSectionLabel" @back="activeSection = null" />
  </div>
  </Transition>

  <!-- 第三栏：子菜单（移动端选中 section 时隐藏，内容整页覆盖） -->
  <div class="mine-list panel-submenu" v-if="activeSection === null || !isMobileLayout">
    <h2 class="mine-list-title">组织设置</h2>
    <div class="mine-list-items">
      <button
        v-for="item in sections"
        :key="item.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeSection === item.key }"
        @click="activeSection = item.key"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="isMobileLayout ? { color: item.color } : undefined"
        ><component :is="item.icon" /></el-icon>
        <b class="settings-section-label">{{ item.label }}</b>
      </button>
    </div>
  </div>

  <!-- 第四栏：当前子项内容（移动端未选 section 时不渲染——否则空白覆盖层会挡住子菜单）；
       Transition 包裹使移动端内容整页覆盖层进出自右滑入、返回向右滑出（桌面端无对应样式，无动画） -->
  <Transition name="settings-overlay-slide">
  <div class="mine-detail" v-if="activeSection !== null || !isMobileLayout">
    <el-empty v-if="loading" description="正在加载组织信息..." />
    <el-empty v-else-if="!organization" description="当前空间组织信息加载失败，请切换空间后重试。" />

    <template v-else>
      <!-- 组织信息：基本字段逐行展示（标签左、值右）+ 副本状态 + 管理员删除操作 -->
      <el-card v-if="activeSection === 'info'" shadow="never" class="panel-card">
        <template #header>
          <h2>组织信息</h2>
        </template>
        <div class="org-info-rows">
          <div class="org-info-row">
            <span class="org-info-label">组织 logo</span>
            <div v-if="editingInfo === 'logo'" class="org-info-edit">
              <AvatarPicker v-model="editInfoValue" :nickname="organization.name" :seed="organization.orgId" :size="48" />
              <el-button size="small" type="primary" :loading="savingInfo" @click="saveInfo">保存</el-button>
              <el-button size="small" @click="cancelEditInfo">取消</el-button>
            </div>
            <span
              v-else
              class="org-info-value"
              :class="{ editable: organization.isCurrentUserAdmin }"
              :title="organization.isCurrentUserAdmin ? '点击编辑' : ''"
              @click="startEditInfo('logo')"
            >
              <OrgAvatar :org-id="organization.orgId" :name="organization.name" :size="32" />
            </span>
          </div>
          <div class="org-info-row">
            <span class="org-info-label">名称</span>
            <div v-if="editingInfo === 'name'" class="org-info-edit">
              <el-input v-model="editInfoValue" size="small" maxlength="60" @keyup.enter="saveInfo" />
              <el-button size="small" type="primary" :loading="savingInfo" @click="saveInfo">保存</el-button>
              <el-button size="small" @click="cancelEditInfo">取消</el-button>
            </div>
            <span
              v-else
              class="org-info-value"
              :class="{ editable: organization.isCurrentUserAdmin }"
              :title="organization.isCurrentUserAdmin ? '点击编辑' : ''"
              @click="startEditInfo('name')"
            >{{ organization.name }}</span>
          </div>
          <div class="org-info-row">
            <span class="org-info-label">描述</span>
            <div v-if="editingInfo === 'description'" class="org-info-edit">
              <el-input v-model="editInfoValue" size="small" maxlength="200" @keyup.enter="saveInfo" />
              <el-button size="small" type="primary" :loading="savingInfo" @click="saveInfo">保存</el-button>
              <el-button size="small" @click="cancelEditInfo">取消</el-button>
            </div>
            <span
              v-else
              class="org-info-value"
              :class="{ editable: organization.isCurrentUserAdmin }"
              :title="organization.isCurrentUserAdmin ? '点击编辑' : ''"
              @click="startEditInfo('description')"
            >{{ organization.description || '暂无描述' }}</span>
          </div>
          <div class="org-info-row">
            <span class="org-info-label">管理人员</span>
            <span class="org-info-value">{{ organization.adminCount }} 人</span>
          </div>
          <div class="org-info-row">
            <span class="org-info-label">成员</span>
            <span class="org-info-value">{{ organization.memberCount }} 人</span>
          </div>
          <div class="org-info-row">
            <span class="org-info-label">最近更新</span>
            <span class="org-info-value">{{ formatDate(organization.updatedAt) }}</span>
          </div>
        </div>

        <div v-if="currentOverview" class="replica-row">
          <el-tag :type="replicaTagType(currentOverview)">{{ replicaLabel(currentOverview) }}</el-tag>
          <span class="replica-hint">
            {{ currentOverview.syncedPeers >= currentOverview.replicaTarget ? '副本充足' : '副本不足，建议成员保持在线或邀请更多节点' }}
            （已同步节点 {{ currentOverview.syncedPeers }} / 成员 {{ currentOverview.totalMembers }}）
          </span>
        </div>

        <div v-if="organization.isCurrentUserAdmin" class="org-actions">
          <el-button type="danger" plain :loading="deleting" @click="deleteOrganization">
            {{ deleting ? '删除中...' : '删除组织' }}
          </el-button>
        </div>
      </el-card>

      <!-- 网关设置：网关编辑（GatewayManager 复用） -->
      <el-card v-else-if="activeSection === 'gateway'" shadow="never" class="panel-card">
        <GatewayManager
          v-model="gatewaySelection"
          :org-id="organization.orgId"
          :members="organization.members"
          :gateways="organization.gateways ?? []"
          :is-admin="organization.isCurrentUserAdmin"
          :saving="savingGateways"
          :valid="gatewaySelectionValid"
          @save="saveGateways"
        />
      </el-card>

      <!-- 找回组织：节点名片分享/导入（RecoverConnectionPanel 复用） -->
      <el-card v-else-if="activeSection === 'recover'" shadow="never" class="panel-card">
        <RecoverConnectionPanel ref="recoverPanelRef" :org-id="organization.orgId" />
      </el-card>

      <!-- 数据治理：内容直接铺在第四栏（PurgeDataPanel，仅管理员） -->
      <el-card v-else-if="activeSection === 'purge'" shadow="never" class="panel-card">
        <template #header>
          <h2>数据治理</h2>
        </template>
        <PurgeDataPanel v-if="organization.isCurrentUserAdmin" :org-id="organization.orgId" />
        <p v-else class="hint">仅管理员可进行数据治理。</p>
      </el-card>

      <!-- 公开设置（PublicOrgPanel 复用：开关 + 展示名 + 组织地址复制） -->
      <el-card v-else-if="activeSection === 'public'" shadow="never" class="panel-card">
        <PublicOrgPanel
          :org="organization"
          v-model:enabled="publicEnabled"
          v-model:display-name="publicDisplayName"
          :saving="savingPublic"
          @save="savePublic"
        />
      </el-card>

      <!-- 发现公开组织（DiscoverOrgsPanel 自带卡片，不再包裹） -->
      <DiscoverOrgsPanel v-else />
    </template>
  </div>
  </Transition>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, watch, type Component } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Brush, Connection, OfficeBuilding, Refresh, Search, Share } from '@element-plus/icons-vue';
import { isMobileLayout } from '../../stores/ui-layout';
import { isOverlayCloseTarget, popOverlay, pushOverlay } from '../../stores/overlay-stack';
import MobileBackBar from '../MobileBackBar.vue';
import { currentSpaceOrgId } from '../../stores/current-space';
import { findOrg, refreshOrganizations } from '../../stores/org-membership';
import { getOrgAvatar, setOrgAvatar } from '../../stores/org-avatars';
import type { OrgSyncOverviewDto as OrgSyncOverview } from '../../api';
import OrgAvatar from '../OrgAvatar.vue';
import AvatarPicker from '../AvatarPicker.vue';
import DiscoverOrgsPanel from './DiscoverOrgsPanel.vue';
import PurgeDataPanel from './PurgeDataPanel.vue';
import GatewayManager from './GatewayManager.vue';
import PublicOrgPanel from './PublicOrgPanel.vue';
import RecoverConnectionPanel from './RecoverConnectionPanel.vue';
import type { OrganizationView } from './types';

type SectionKey = 'info' | 'gateway' | 'recover' | 'purge' | 'public' | 'discover';

export default defineComponent({
  name: 'OrgSettingsPanel',
  components: {
    DiscoverOrgsPanel,
    PurgeDataPanel,
    GatewayManager,
    PublicOrgPanel,
    RecoverConnectionPanel,
    OrgAvatar,
    AvatarPicker
  },
  emits: ['section-toggle'],
  setup(_, { emit }) {
    // 移动端（Android 前端改造）：子菜单整页 <-> 内容整页覆盖层；桌面端保持分栏
    const activeSection = ref<SectionKey | null>(isMobileLayout.value ? null : 'info');
    // 覆盖层登记（token 制）：移动端选中 section 时入栈，返回子菜单/卸载时出栈；
    // 系统回退键仅关栈顶（叠层时逐层回退，不会"跳两层"）
    let overlayToken: symbol | null = null;
    const releaseOverlay = () => {
      popOverlay(overlayToken);
      overlayToken = null;
    };
    watch(
      () => activeSection.value !== null && isMobileLayout.value,
      (opened) => {
        // 告知宿主页隐藏页级返回栏（面板自带返回栏接管），避免双栏叠层、内容偏下
        emit('section-toggle', opened);
        if (opened && !overlayToken) {
          overlayToken = pushOverlay();
        } else if (!opened) {
          releaseOverlay();
        }
      },
      { immediate: true }
    );
    onBeforeUnmount(releaseOverlay);
    const onCloseOverlay = (event: Event) => {
      if (isOverlayCloseTarget(event, overlayToken) && activeSection.value !== null && isMobileLayout.value) {
        activeSection.value = null;
      }
    };
    onMounted(() => window.addEventListener('spark:close-overlay', onCloseOverlay));
    onBeforeUnmount(() => window.removeEventListener('spark:close-overlay', onCloseOverlay));
    const organization = ref<OrganizationView | null>(null);
    const overview = ref<OrgSyncOverview | null>(null);
    const loading = ref(false);

    // 组织信息内联编辑（logo/名称/描述，仅管理员）：点击值进入编辑，保存/取消收尾
    const editingInfo = ref<'' | 'logo' | 'name' | 'description'>('');
    const editInfoValue = ref('');
    const savingInfo = ref(false);

    const deleting = ref(false);
    const gatewaySelection = ref<string[]>([]);
    const savingGateways = ref(false);

    // 公开组织（§15/§16）：管理员开关 + 展示名；组织地址全成员可见可复制
    const publicEnabled = ref(false);
    const publicDisplayName = ref('');
    const savingPublic = ref(false);

    // 找回组织面板引用：切换组织时重置其内部状态
    const recoverPanelRef = ref<{ reset: () => void } | null>(null);

    // color 为移动端菜单图标色（微信式每项一色，取色与 utils/palette 品牌色板同源，桌面端不使用）
    const sections: Array<{ key: SectionKey; label: string; icon: Component; color: string }> = [
      { key: 'info', label: '组织信息', icon: OfficeBuilding, color: '#00b8a9' },
      { key: 'gateway', label: '网关设置', icon: Connection, color: '#3296fa' },
      { key: 'recover', label: '找回组织', icon: Refresh, color: '#34c19b' },
      { key: 'purge', label: '数据治理', icon: Brush, color: '#7b61ff' },
      { key: 'public', label: '公开设置', icon: Share, color: '#ff7d00' },
      { key: 'discover', label: '发现公开组织', icon: Search, color: '#eb2f96' }
    ];

    /** 当前选中子菜单的标题（移动端整页返回栏标题） */
    const activeSectionLabel = computed(
      () => sections.find((sec) => sec.key === activeSection.value)?.label ?? ''
    );

    const currentOverview = computed(() => overview.value);

    const gatewaySelectionValid = computed(() => {
      if (!organization.value) {
        return false;
      }
      const memberIds = new Set(organization.value.members.map((member) => member.rootId));
      return (
        gatewaySelection.value.length >= 2 &&
        gatewaySelection.value.length <= 3 &&
        gatewaySelection.value.every((rootId) => memberIds.has(rootId))
      );
    });

    // 只加载当前空间组织：org-membership 缓存里找对应 orgId，顺带取同步概览
    const reload = async () => {
      const orgId = currentSpaceOrgId.value;
      if (!orgId) {
        organization.value = null;
        overview.value = null;
        return;
      }
      loading.value = true;
      try {
        await refreshOrganizations();
        const found = findOrg(orgId);
        organization.value = found;
        if (found) {
          // 初始化网关选择/公开设置，避免残留上一个组织的数据
          gatewaySelection.value = [...(found.gateways ?? [])];
          publicEnabled.value = found.isPublic ?? false;
          publicDisplayName.value = found.orgDisplayName ?? '';
          try {
            overview.value = await window.electronAPI.organization.getSyncOverview(found.orgId);
          } catch {
            overview.value = null;
          }
        } else {
          overview.value = null;
        }
      } catch (error) {
        ElMessage.error(`加载组织失败：${error}`);
        organization.value = null;
        overview.value = null;
      } finally {
        loading.value = false;
      }
    };

    /**
     * 写操作前网络检查（development_plan「组织网络状态 UI」）：
     * 组织网络丢失/仅本地时提示「数据将在恢复后同步」——只提示，不阻断。
     */
    const notifyIfNetworkUnavailable = async () => {
      if (!organization.value) {
        return;
      }
      try {
        const item = await window.electronAPI.organization.getSyncOverview(organization.value.orgId);
        if (item && (item.status === 'lost' || item.status === 'localOnly')) {
          ElMessage.warning('当前组织网络不可用，数据将在恢复后同步');
        }
      } catch {
        // 状态读取失败不阻断操作
      }
    };

    const replicaLabel = (item: OrgSyncOverview | null) => {
      if (!item) {
        return '';
      }
      return `副本 ${item.syncedPeers}/${item.replicaTarget}`;
    };

    const replicaTagType = (item: OrgSyncOverview | null) => {
      if (!item) {
        return 'info';
      }
      return item.syncedPeers >= item.replicaTarget ? 'success' : 'warning';
    };

    const startEditInfo = (field: 'logo' | 'name' | 'description') => {
      if (!organization.value?.isCurrentUserAdmin) {
        return;
      }
      editingInfo.value = field;
      if (field === 'logo') {
        editInfoValue.value = getOrgAvatar(organization.value.orgId);
      } else {
        editInfoValue.value = field === 'name' ? organization.value.name : organization.value.description;
      }
    };

    const cancelEditInfo = () => {
      editingInfo.value = '';
    };

    const saveInfo = async () => {
      if (!organization.value || !editingInfo.value) {
        return;
      }
      const field = editingInfo.value;
      // 组织 logo：走内核 organization.updateInfo 持久化（同步给其他成员），
      // 成功后写入本地 org-avatars 展示缓存（空串 = 清除 logo）
      if (field === 'logo') {
        savingInfo.value = true;
        try {
          organization.value = await window.electronAPI.organization.updateInfo(organization.value.orgId, {
            avatar: editInfoValue.value
          });
          setOrgAvatar(organization.value.orgId, editInfoValue.value);
          editingInfo.value = '';
          ElMessage.success('已保存');
        } catch (error) {
          ElMessage.error(`保存失败：${error}`);
        } finally {
          savingInfo.value = false;
        }
        return;
      }
      const value = editInfoValue.value.trim();
      if (field === 'name' && !value) {
        ElMessage.warning('名称不能为空');
        return;
      }
      savingInfo.value = true;
      try {
        organization.value = await window.electronAPI.organization.updateInfo(organization.value.orgId, {
          [field]: value
        });
        editingInfo.value = '';
        ElMessage.success('已保存');
      } catch (error) {
        ElMessage.error(`保存失败：${error}`);
      } finally {
        savingInfo.value = false;
      }
    };

    const saveGateways = async () => {
      if (!organization.value || !gatewaySelectionValid.value) {
        ElMessage.warning('请选择 2-3 名本组织成员作为网关');
        return;
      }
      savingGateways.value = true;
      try {
        await notifyIfNetworkUnavailable();
        await window.electronAPI.organization.setGateways(organization.value.orgId, gatewaySelection.value);
        ElMessage.success('网关设置已保存');
        await reload();
      } catch (error) {
        ElMessage.error(`保存网关设置失败：${error}`);
      } finally {
        savingGateways.value = false;
      }
    };

    const savePublic = async () => {
      if (!organization.value) {
        return;
      }
      savingPublic.value = true;
      try {
        await window.electronAPI.organization.setPublic(
          organization.value.orgId,
          publicEnabled.value,
          publicDisplayName.value.trim() || undefined
        );
        ElMessage.success(publicEnabled.value ? '组织已公开' : '组织已取消公开');
        await reload();
      } catch (error) {
        ElMessage.error(`保存公开设置失败：${error}`);
      } finally {
        savingPublic.value = false;
      }
    };

    const deleteOrganization = async () => {
      if (!organization.value) {
        return;
      }

      try {
        await ElMessageBox.confirm(`确认删除组织「${organization.value.name}」？`, '删除确认', {
          type: 'warning',
          confirmButtonText: '确认删除',
          cancelButtonText: '取消'
        });
      } catch {
        return;
      }

      deleting.value = true;
      try {
        await window.electronAPI.organization.delete(organization.value.orgId);
        ElMessage.success('组织已删除');
        // 本组织已消失：reload 后找不到记录，第四栏自然落入空态
        await reload();
      } catch (error) {
        ElMessage.error(`删除组织失败：${error}`);
      } finally {
        deleting.value = false;
      }
    };

    const formatDate = (timestamp: number) => {
      return new Intl.DateTimeFormat('zh-CN', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
      }).format(new Date(timestamp));
    };

    onMounted(() => {
      void reload();
    });

    // 空间切换后重新加载当前组织，并重置找回组织面板与内联编辑态
    watch(currentSpaceOrgId, () => {
      recoverPanelRef.value?.reset();
      editingInfo.value = '';
      void reload();
    });

    return {
      activeSection,
      activeSectionLabel,
      isMobileLayout,
      sections,
      organization,
      currentOverview,
      loading,
      editingInfo,
      editInfoValue,
      savingInfo,
      deleting,
      gatewaySelection,
      savingGateways,
      publicEnabled,
      publicDisplayName,
      savingPublic,
      recoverPanelRef,
      gatewaySelectionValid,
      reload,
      replicaLabel,
      replicaTagType,
      startEditInfo,
      cancelEditInfo,
      saveInfo,
      saveGateways,
      savePublic,
      deleteOrganization,
      formatDate
    };
  }
});
</script>

<style scoped>
/* 子菜单项标签：常规字重，选中行加粗（同 SystemSettingsPanel） */
.settings-section-label {
  font-size: 14px;
  font-weight: 400;
}

.mine-list-item.active .settings-section-label {
  font-weight: 600;
}

/* 组织信息字段行：标签左、值右，行间细分隔线 */
.org-info-rows {
  display: flex;
  flex-direction: column;
}

.org-info-row {
  display: flex;
  min-height: 44px;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 8px 0;
  border-bottom: 1px solid var(--spark-border-light);
}

.org-info-row:last-child {
  border-bottom: 0;
}

.org-info-label {
  flex-shrink: 0;
  color: var(--spark-text-2);
  font-size: 14px;
}

.org-info-value {
  min-width: 0;
  color: var(--spark-text-1);
  font-size: 14px;
  text-align: right;
  word-break: break-all;
}

/* 管理员可编辑的字段值：悬停提示可点 */
.org-info-value.editable {
  cursor: pointer;
}

.org-info-value.editable:hover {
  color: var(--spark-primary);
}

/* 内联编辑区：输入框 + 保存/取消，靠右对齐（与值同侧） */
.org-info-edit {
  display: flex;
  flex: 1;
  min-width: 0;
  justify-content: flex-end;
  gap: 8px;
}

.org-info-edit .el-input {
  max-width: 280px;
}

/* 副本状态行 / 管理员操作区：原 org.css 的 .replica-row / .drawer-actions 的组件级副本 */
.replica-row {
  margin-top: 12px;
  display: flex;
  align-items: center;
  gap: 10px;
}

.replica-hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.org-actions {
  margin-top: 16px;
  display: flex;
  gap: 8px;
}

/* ---- 移动端（≤768px，Android 前端改造）：子菜单整页 + 内容整页覆盖 ---- */
@media (max-width: 768px) {
  .org-settings-panel {
    position: relative;
    flex: 1;
    min-height: 0;
    width: 100%;
  }

  /* 子菜单（直接子级 mine-list）整页占满 */
  .org-settings-panel > .mine-list.panel-submenu {
    flex: 1;
    height: 100%;
  }

  /* 内容区（mine-detail）整页覆盖；top 让出返回栏高度（48px + 状态栏安全区），
     避免首行内容被返回栏遮挡；进出动画由 Transition（settings-overlay-slide）承担 */
  .org-settings-panel > .mine-detail {
    position: absolute;
    top: calc(48px + env(safe-area-inset-top, 0px));
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 2;
    background: var(--spark-bg-card);
    overflow-y: auto;
  }

  /* 返回栏：浮在面板顶部（与内容同向滑动，视觉等同整页推入/推出） */
  .settings-mobile-bar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 3;
  }

  /* 覆盖层进出动画（与导航栈 push/pop 同款曲线与方向：进=自右滑入，出=向右滑出） */
  .settings-overlay-slide-enter-active,
  .settings-overlay-slide-leave-active {
    transition: transform 260ms cubic-bezier(0.25, 0.46, 0.45, 0.94);
    will-change: transform;
  }

  .settings-overlay-slide-enter-from,
  .settings-overlay-slide-leave-to {
    transform: translateX(100%);
  }

  /* 移动端子菜单字号按微信调大：主文字 16px；移动端菜单是页面切换不是选中，选中态不生效 */
  .settings-section-label {
    font-size: 16px;
  }

  .mine-list-item.active .settings-section-label {
    font-weight: 400;
  }
}
</style>
