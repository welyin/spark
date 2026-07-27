<!-- 组织页骨架：组织列表 + 详情抽屉结构 + 各面板/对话框的状态编排（面板实现见 src/components/org/） -->
<template>
  <section class="org-page">
    <header class="page-header">
      <div class="page-header-main">
        <p class="eyebrow">组织管理</p>
        <h1>组织</h1>
        <p class="lede">创建组织后你会默认成为管理员。邀请成员先预录入 RootID，再把邀请码发给对方。</p>
        <div class="header-stats">
          <span class="stat-chip">我所属的组织 <b>{{ organizations.length }}</b></span>
          <span class="stat-chip">我担任管理员 <b>{{ adminOrgCount }}</b></span>
        </div>
      </div>
      <div class="page-header-actions">
        <el-button type="primary" @click="createDialogVisible = true">创建组织</el-button>
        <el-button @click="joinDialogVisible = true">邀请码加入</el-button>
        <el-button text type="primary" @click="refreshOrganizations">刷新</el-button>
      </div>
    </header>

    <el-card shadow="never" class="panel-card">
      <template #header>
        <h2>我的组织</h2>
      </template>

      <el-empty v-if="loading" description="正在加载组织..." />
      <el-empty v-else-if="organizations.length === 0" description="你还没有加入任何组织。创建或凭邀请码加入一个吧。" />
      <div v-else class="org-grid">
        <el-card
          v-for="organization in organizations"
          :key="organization.orgId"
          shadow="hover"
          class="org-item"
          @click="openDetail(organization)"
        >
          <div class="org-item-top">
            <strong>{{ organization.name }}</strong>
            <el-tag :type="organization.isCurrentUserAdmin ? 'danger' : 'info'">
              {{ organization.isCurrentUserAdmin ? '管理员' : '成员' }}
            </el-tag>
          </div>
          <p class="org-desc">{{ organization.description || '暂无描述' }}</p>
          <div class="org-meta">
            <span>{{ organization.memberCount }} 人</span>
            <span>{{ organization.adminCount }} 管理员</span>
            <el-tag
              v-if="overviewOf(organization.orgId)"
              size="small"
              :type="replicaTagType(overviewOf(organization.orgId))"
            >
              {{ replicaLabel(overviewOf(organization.orgId)) }}
            </el-tag>
          </div>
          <div class="org-item-actions">
            <el-button
              v-if="organization.basePluginDomain"
              text
              type="primary"
              size="small"
              @click.stop="openOrgPlugin(organization)"
            >
              打开插件
            </el-button>
            <span class="org-plugin-domain">{{ organization.basePluginDomain || '-' }}</span>
          </div>
        </el-card>
      </div>
    </el-card>

    <DiscoverOrgsPanel />

    <CreateOrgDialog
      ref="createDialogRef"
      v-model="createDialogVisible"
      :creating="creating"
      :foundation-plugins="foundationPlugins"
      @submit="createOrganization"
    />

    <JoinOrgDialog
      v-model="joinDialogVisible"
      :joining="joining"
      @submit="acceptInvite"
    />

    <InviteMemberDialog
      v-model="inviteDialogVisible"
      :org-id="selectedOrgId"
      :before-write="notifyIfNetworkUnavailable"
      :on-invited="refreshOrganizations"
    />

    <PurgeDataDialog v-model="purgeDialogVisible" :org-id="selectedOrgId" />

    <!-- 组织详情抽屉 -->
    <el-drawer v-model="drawerVisible" size="min(640px, 94%)" :with-header="false">
      <div v-if="selectedOrganization" class="drawer-body">
        <div class="drawer-header">
          <div>
            <p class="eyebrow">组织详情</p>
            <h2>{{ selectedOrganization.name }}</h2>
            <p class="lede">{{ selectedOrganization.description || '暂无描述' }}</p>
          </div>
          <el-tag :type="selectedOrganization.isCurrentUserAdmin ? 'danger' : 'info'">
            {{ selectedOrganization.isCurrentUserAdmin ? '管理员' : '成员' }}
          </el-tag>
        </div>

        <el-descriptions :column="2" border>
          <el-descriptions-item label="组织 ID">{{ selectedOrganization.orgId }}</el-descriptions-item>
          <el-descriptions-item label="创建者">{{ selectedOrganization.createdBy }}</el-descriptions-item>
          <el-descriptions-item label="基础插件">{{ selectedOrganization.basePluginDomain || '-' }}</el-descriptions-item>
          <el-descriptions-item label="成员数">{{ selectedOrganization.memberCount }}</el-descriptions-item>
          <el-descriptions-item label="管理员数">{{ selectedOrganization.adminCount }}</el-descriptions-item>
          <el-descriptions-item label="最近更新">{{ formatDate(selectedOrganization.updatedAt) }}</el-descriptions-item>
        </el-descriptions>

        <div v-if="currentOverview" class="replica-row">
          <el-tag :type="replicaTagType(currentOverview)">{{ replicaLabel(currentOverview) }}</el-tag>
          <span class="replica-hint">
            {{ currentOverview.syncedPeers >= currentOverview.replicaTarget ? '副本充足' : '副本不足，建议成员保持在线或邀请更多节点' }}
            （已同步节点 {{ currentOverview.syncedPeers }} / 成员 {{ currentOverview.totalMembers }}）
          </span>
        </div>

        <MemberList
          :members="selectedOrganization.members"
          :is-admin="selectedOrganization.isCurrentUserAdmin"
          :current-root-id="currentRootId"
          :removing-root-id="removingRootId"
          :member-sync-label="memberSyncLabel"
          :format-date="formatDate"
          @remove="removeMember"
        />

        <GatewayManager
          v-model="gatewaySelection"
          :members="selectedOrganization.members"
          :gateways="currentGateways"
          :is-admin="selectedOrganization.isCurrentUserAdmin"
          :saving="savingGateways"
          :valid="gatewaySelectionValid"
          @save="saveGateways"
        />

        <PublicOrgPanel
          :org="selectedOrganization"
          v-model:enabled="publicEnabled"
          v-model:display-name="publicDisplayName"
          :saving="savingPublic"
          @save="savePublic"
        />

        <RecoverConnectionPanel ref="recoverPanelRef" :org-id="selectedOrganization.orgId" />

        <div v-if="selectedOrganization.isCurrentUserAdmin" class="drawer-actions">
          <el-button type="primary" @click="inviteDialogVisible = true">邀请成员</el-button>
          <el-button v-if="selectedOrganization.basePluginDomain" @click="openOrgPlugin(selectedOrganization)">
            打开插件
          </el-button>
          <el-button v-if="selectedOrganization.basePluginDomain" @click="purgeDialogVisible = true">数据治理</el-button>
          <el-button type="danger" plain :loading="deleting" @click="deleteOrganization">
            {{ deleting ? '删除中...' : '删除组织' }}
          </el-button>
        </div>
        <el-alert
          v-else
          title="当前用户不是管理员，只能查看成员列表。"
          type="warning"
          :closable="false"
          show-icon
          class="drawer-alert"
        />
      </div>
    </el-drawer>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { currentOrgId } from '../stores/current-org';
import type { OrgSyncOverviewDto as OrgSyncOverview } from '../api';
import DiscoverOrgsPanel from '../components/org/DiscoverOrgsPanel.vue';
import CreateOrgDialog from '../components/org/CreateOrgDialog.vue';
import JoinOrgDialog from '../components/org/JoinOrgDialog.vue';
import InviteMemberDialog from '../components/org/InviteMemberDialog.vue';
import PurgeDataDialog from '../components/org/PurgeDataDialog.vue';
import MemberList from '../components/org/MemberList.vue';
import GatewayManager from '../components/org/GatewayManager.vue';
import PublicOrgPanel from '../components/org/PublicOrgPanel.vue';
import RecoverConnectionPanel from '../components/org/RecoverConnectionPanel.vue';
import type { CreateForm, OrganizationMember, OrganizationView, PluginCatalogItem } from '../components/org/types';

export default defineComponent({
  name: 'OrgPage',
  components: {
    DiscoverOrgsPanel,
    CreateOrgDialog,
    JoinOrgDialog,
    InviteMemberDialog,
    PurgeDataDialog,
    MemberList,
    GatewayManager,
    PublicOrgPanel,
    RecoverConnectionPanel
  },
  emits: ['open-plugin-tab'],
  setup(_, { emit }) {
    const organizations = ref<OrganizationView[]>([]);
    const overviews = ref<Record<string, OrgSyncOverview | null>>({});
    const selectedOrgId = currentOrgId;
    const currentRootId = ref('');
    const loading = ref(false);

    const createDialogVisible = ref(false);
    const joinDialogVisible = ref(false);
    const inviteDialogVisible = ref(false);
    const purgeDialogVisible = ref(false);
    const drawerVisible = ref(false);

    const creating = ref(false);
    const joining = ref(false);
    const deleting = ref(false);
    const removingRootId = ref('');
    const gatewaySelection = ref<string[]>([]);
    const savingGateways = ref(false);

    // 公开组织（§15/§16）：管理员开关 + 展示名；组织地址全成员可见可复制
    const publicEnabled = ref(false);
    const publicDisplayName = ref('');
    const savingPublic = ref(false);

    // 子组件引用：创建对话框（成功后重置表单）、恢复连接面板（打开详情时重置）
    const createDialogRef = ref<{ resetAfterCreate: () => void } | null>(null);
    const recoverPanelRef = ref<{ reset: () => void } | null>(null);

    const pluginCatalog = ref<PluginCatalogItem[]>([]);

    const foundationPlugins = computed(() => {
      return pluginCatalog.value.filter((plugin) => plugin.category === 'foundation');
    });

    const selectedOrganization = computed(() => {
      return organizations.value.find((organization) => organization.orgId === selectedOrgId.value) ?? null;
    });

    const currentOverview = computed(() => {
      return selectedOrgId.value ? overviews.value[selectedOrgId.value] ?? null : null;
    });

    const adminOrgCount = computed(() => {
      return organizations.value.filter((organization) => organization.isCurrentUserAdmin).length;
    });

    const currentGateways = computed(() => {
      return selectedOrganization.value?.gateways ?? [];
    });

    const gatewaySelectionValid = computed(() => {
      if (!selectedOrganization.value) {
        return false;
      }
      const memberIds = new Set(selectedOrganization.value.members.map((member) => member.rootId));
      return (
        gatewaySelection.value.length >= 2 &&
        gatewaySelection.value.length <= 3 &&
        gatewaySelection.value.every((rootId) => memberIds.has(rootId))
      );
    });

    const saveGateways = async () => {
      if (!selectedOrganization.value || !gatewaySelectionValid.value) {
        ElMessage.warning('请选择 2-3 名本组织成员作为网关');
        return;
      }
      savingGateways.value = true;
      try {
        await notifyIfNetworkUnavailable();
        await window.electronAPI.organization.setGateways(selectedOrganization.value.orgId, gatewaySelection.value);
        ElMessage.success('网关设置已保存');
        await refreshOrganizations();
      } catch (error) {
        ElMessage.error(`保存网关设置失败：${error}`);
      } finally {
        savingGateways.value = false;
      }
    };

    const overviewOf = (orgId: string) => {
      return overviews.value[orgId] ?? null;
    };

    const replicaLabel = (overview: OrgSyncOverview | null) => {
      if (!overview) {
        return '';
      }
      return `副本 ${overview.syncedPeers}/${overview.replicaTarget}`;
    };

    const replicaTagType = (overview: OrgSyncOverview | null) => {
      if (!overview) {
        return 'info';
      }
      return overview.syncedPeers >= overview.replicaTarget ? 'success' : 'warning';
    };

    /**
     * 写操作前网络检查（development_plan「组织网络状态 UI」）：
     * 组织网络丢失/仅本地时提示「数据将在恢复后同步」——只提示，不阻断。
     */
    const notifyIfNetworkUnavailable = async () => {
      if (!selectedOrgId.value) {
        return;
      }
      try {
        const overview = await window.electronAPI.organization.getSyncOverview(selectedOrgId.value);
        if (overview && (overview.status === 'lost' || overview.status === 'localOnly')) {
          ElMessage.warning('当前组织网络不可用，数据将在恢复后同步');
        }
      } catch {
        // 状态读取失败不阻断操作
      }
    };

    const loadCurrentRootId = async () => {
      try {
        const status = await window.electronAPI.rootIdentity.status();
        currentRootId.value = status.rootId ?? '';
      } catch {
        currentRootId.value = '';
      }
    };

    const refreshOrganizations = async () => {
      loading.value = true;
      try {
        organizations.value = await window.electronAPI.organization.listMine();
        if (!organizations.value.some((organization) => organization.orgId === selectedOrgId.value)) {
          selectedOrgId.value = organizations.value[0]?.orgId ?? '';
        }
        const entries = await Promise.all(
          organizations.value.map(async (organization) => {
            try {
              return [organization.orgId, await window.electronAPI.organization.getSyncOverview(organization.orgId)] as const;
            } catch {
              return [organization.orgId, null] as const;
            }
          })
        );
        overviews.value = Object.fromEntries(entries);
      } catch (error) {
        ElMessage.error(`加载组织失败：${error}`);
      } finally {
        loading.value = false;
      }
    };

    const loadPluginCatalog = async () => {
      try {
        pluginCatalog.value = await window.electronAPI.plugin.listCatalog();
      } catch (error) {
        ElMessage.error(`加载插件目录失败：${error}`);
      }
    };

    const openDetail = (organization: OrganizationView) => {
      selectedOrgId.value = organization.orgId;
      gatewaySelection.value = [...(organization.gateways ?? [])];
      publicEnabled.value = organization.isPublic ?? false;
      publicDisplayName.value = organization.orgDisplayName ?? '';
      recoverPanelRef.value?.reset();
      drawerVisible.value = true;
    };

    const openOrgPlugin = (organization: OrganizationView) => {
      if (!organization.basePluginDomain) {
        return;
      }
      emit('open-plugin-tab', {
        pluginDomain: organization.basePluginDomain,
        pluginView: 'default',
        title: `${organization.name} · 插件`,
        icon: '基',
        pluginContext: {
          orgId: organization.orgId
        }
      });
    };

    const createOrganization = async (form: CreateForm) => {
      if (!form.name.trim()) {
        ElMessage.warning('请输入组织名称');
        return;
      }
      if (!form.basePluginDomain) {
        ElMessage.warning('请选择基础插件');
        return;
      }

      creating.value = true;
      try {
        const created = await window.electronAPI.organization.create({
          name: form.name,
          description: form.description,
          basePluginDomain: form.basePluginDomain
        });
        ElMessage.success(`组织已创建：${created.name}`);
        createDialogRef.value?.resetAfterCreate();
        createDialogVisible.value = false;
        await refreshOrganizations();
        // 走 openDetail 初始化抽屉状态（网关选择/公开设置/恢复面板），避免残留上一个组织的数据
        const createdView = organizations.value.find((item) => item.orgId === created.orgId);
        if (createdView) {
          openDetail(createdView);
        }
      } catch (error) {
        ElMessage.error(`创建组织失败：${error}`);
      } finally {
        creating.value = false;
      }
    };

    const acceptInvite = async (code: string) => {
      if (!code.trim()) {
        ElMessage.warning('请输入邀请码');
        return;
      }

      joining.value = true;
      try {
        const joined = await window.electronAPI.organization.acceptInvite(code.trim());
        ElMessage.success(`已加入组织：${joined.orgName}`);
        joinDialogVisible.value = false;
        await refreshOrganizations();
        // 同上：走 openDetail 初始化抽屉状态，避免残留上一个组织的数据
        const joinedView = organizations.value.find((item) => item.orgId === joined.orgId);
        if (joinedView) {
          openDetail(joinedView);
        }
      } catch (error) {
        ElMessage.error(`加入组织失败：${error}`);
      } finally {
        joining.value = false;
      }
    };

    const savePublic = async () => {
      if (!selectedOrganization.value) {
        return;
      }
      savingPublic.value = true;
      try {
        await window.electronAPI.organization.setPublic(
          selectedOrganization.value.orgId,
          publicEnabled.value,
          publicDisplayName.value.trim() || undefined
        );
        ElMessage.success(publicEnabled.value ? '组织已公开' : '组织已取消公开');
        await refreshOrganizations();
      } catch (error) {
        ElMessage.error(`保存公开设置失败：${error}`);
      } finally {
        savingPublic.value = false;
      }
    };

    const removeMember = async (member: OrganizationMember) => {
      if (!selectedOrganization.value) {
        return;
      }

      try {
        await ElMessageBox.confirm(`确认将成员「${member.rootId.slice(0, 12)}...」移出组织？`, '移除确认', {
          type: 'warning',
          confirmButtonText: '确认移除',
          cancelButtonText: '取消'
        });
      } catch {
        return;
      }

      removingRootId.value = member.rootId;
      try {
        await notifyIfNetworkUnavailable();
        await window.electronAPI.organization.removeMember(selectedOrganization.value.orgId, member.rootId);
        ElMessage.success('成员已移除');
        await refreshOrganizations();
      } catch (error) {
        ElMessage.error(`移除成员失败：${error}`);
      } finally {
        removingRootId.value = '';
      }
    };

    const deleteOrganization = async () => {
      if (!selectedOrganization.value) {
        return;
      }

      try {
        await ElMessageBox.confirm(`确认删除组织「${selectedOrganization.value.name}」？`, '删除确认', {
          type: 'warning',
          confirmButtonText: '确认删除',
          cancelButtonText: '取消'
        });
      } catch {
        return;
      }

      deleting.value = true;
      try {
        await window.electronAPI.organization.delete(selectedOrganization.value.orgId);
        ElMessage.success('组织已删除');
        drawerVisible.value = false;
        await refreshOrganizations();
      } catch (error) {
        ElMessage.error(`删除组织失败：${error}`);
      } finally {
        deleting.value = false;
      }
    };

    const memberSyncLabel = (rootId: string) => {
      const item = currentOverview.value?.members.find((member) => member.rootId === rootId);
      if (!item) {
        return '-';
      }
      if (item.isSelf) {
        return '本机';
      }
      if (!item.everSynced) {
        return '未同步';
      }
      return item.lastSyncedAt ? formatDate(item.lastSyncedAt) : '已同步';
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
      void loadCurrentRootId();
      void loadPluginCatalog();
      void refreshOrganizations();
    });

    return {
      organizations,
      overviewOf,
      selectedOrgId,
      selectedOrganization,
      currentOverview,
      currentRootId,
      loading,
      createDialogVisible,
      joinDialogVisible,
      inviteDialogVisible,
      purgeDialogVisible,
      drawerVisible,
      creating,
      joining,
      deleting,
      removingRootId,
      gatewaySelection,
      savingGateways,
      publicEnabled,
      publicDisplayName,
      savingPublic,
      createDialogRef,
      recoverPanelRef,
      currentGateways,
      gatewaySelectionValid,
      saveGateways,
      foundationPlugins,
      adminOrgCount,
      replicaLabel,
      replicaTagType,
      notifyIfNetworkUnavailable,
      refreshOrganizations,
      openDetail,
      openOrgPlugin,
      createOrganization,
      acceptInvite,
      savePublic,
      removeMember,
      deleteOrganization,
      memberSyncLabel,
      formatDate
    };
  }
});
</script>

<!-- 全局引入（非 scoped）：面板已拆到子组件，scoped 会导致 .org-page h2/h3 等后代选择器失效；类名与规则不变 -->
<style src="../styles/pages/org.css"></style>
