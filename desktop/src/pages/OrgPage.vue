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
        <el-button type="primary" @click="openCreateDialog">创建组织</el-button>
        <el-button @click="openJoinDialog">邀请码加入</el-button>
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

    <!-- 创建组织对话框 -->
    <el-dialog v-model="createDialogVisible" title="创建组织" width="520px">
      <el-form label-position="top">
        <el-form-item label="组织名称">
          <el-input v-model="createForm.name" placeholder="例如：产品组" />
        </el-form-item>
        <el-form-item label="基础插件">
          <el-select v-model="createForm.basePluginDomain" placeholder="请选择组织基础插件" style="width: 100%">
            <el-option
              v-for="plugin in foundationPlugins"
              :key="plugin.domain"
              :label="`${plugin.name} (${plugin.domain})`"
              :value="plugin.domain"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="组织描述">
          <el-input
            v-model="createForm.description"
            type="textarea"
            :rows="3"
            placeholder="可选，描述组织用途"
          />
        </el-form-item>
      </el-form>
      <p class="hint">创建人会自动成为该组织的管理员和首位成员。组织必须绑定一个基础插件。</p>
      <template #footer>
        <el-button @click="createDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="creating" @click="createOrganization">
          {{ creating ? '创建中...' : '创建组织' }}
        </el-button>
      </template>
    </el-dialog>

    <!-- 邀请码加入对话框 -->
    <el-dialog v-model="joinDialogVisible" title="通过邀请码加入" width="520px">
      <el-form label-position="top">
        <el-form-item label="邀请码">
          <el-input
            v-model="joinCode"
            type="textarea"
            :rows="4"
            placeholder="粘贴管理员分享给你的邀请码"
          />
        </el-form-item>
      </el-form>
      <p class="hint">加入前提：管理员已先将你的 RootID 录入组织成员。邀请码 24 小时内有效，用于连接管理员节点并拉取组织数据。</p>
      <template #footer>
        <el-button @click="joinDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="joining" @click="acceptInvite">
          {{ joining ? '加入中...' : '加入组织' }}
        </el-button>
      </template>
    </el-dialog>

    <!-- 邀请成员对话框（两步：预录入 -> 生成邀请码） -->
    <el-dialog v-model="inviteDialogVisible" title="邀请成员" width="520px" @closed="resetInviteDialog">
      <template v-if="!inviteResult">
        <el-form label-position="top">
          <el-form-item label="成员 RootID">
            <el-input v-model="inviteRootId" placeholder="64 位 RootID" />
          </el-form-item>
          <el-collapse class="invite-advanced">
            <el-collapse-item title="高级选项：对方节点信息（可选）" name="advanced">
              <el-form-item label="成员 PeerId（可选）">
                <el-input v-model="invitePeerId" placeholder="例如：12D3KooW..." />
              </el-form-item>
              <el-form-item label="成员节点地址（可选，可多条，逗号/分号/换行分隔）">
                <el-input
                  v-model="inviteAddresses"
                  type="textarea"
                  :rows="3"
                  placeholder="例如：/ip4/127.0.0.1/tcp/15002/ws"
                />
              </el-form-item>
            </el-collapse-item>
          </el-collapse>
        </el-form>
        <p class="hint">只填 RootID 即可预录入；对方凭邀请码加入时会自动回填节点地址。</p>
      </template>
      <template v-else>
        <el-alert title="成员已预录入，邀请码已生成" type="success" :closable="false" show-icon />
        <el-form label-position="top" class="invite-result">
          <el-form-item label="邀请码（24 小时内有效）">
            <el-input v-model="inviteResult" type="textarea" :rows="4" readonly />
          </el-form-item>
        </el-form>
        <p class="hint">请通过线下渠道（微信/当面）把邀请码发给对方；对方凭码连接你的节点完成加入，期间你需要保持在线。</p>
      </template>
      <template #footer>
        <template v-if="!inviteResult">
          <el-button @click="inviteDialogVisible = false">取消</el-button>
          <el-button type="primary" :loading="inviting" @click="addMemberAndInvite">
            {{ inviting ? '处理中...' : '添加并生成邀请码' }}
          </el-button>
        </template>
        <template v-else>
          <el-button @click="inviteDialogVisible = false">完成</el-button>
          <el-button type="primary" @click="copyInvite">复制邀请码</el-button>
        </template>
      </template>
    </el-dialog>

    <!-- 数据治理对话框（管理员手动清理本机旧数据） -->
    <el-dialog v-model="purgeDialogVisible" title="数据治理（清理本机旧数据）" width="560px" @closed="resetPurgeDialog">
      <template v-if="!purgeResult">
        <el-alert
          title="只清理本机指定日期之前的旧数据；组织数据仍保留在其他成员副本中，且清理后同时代数据不会再被同步回本机。"
          type="info"
          :closable="false"
          show-icon
          class="purge-tip"
        />
        <el-form label-position="top">
          <el-form-item label="清理该日期之前的数据">
            <el-date-picker
              v-model="purgeBeforeDate"
              type="date"
              placeholder="选择日期"
              :disabled-date="disableFutureDate"
              style="width: 100%"
            />
          </el-form-item>
        </el-form>
        <div class="purge-actions">
          <el-button :loading="purgePreviewing" :disabled="!purgeBeforeDate" @click="runPurgePreview">预览影响</el-button>
          <el-button :loading="purgeExporting" @click="exportBeforePurge">导出数据</el-button>
        </div>
        <template v-if="purgePreview">
          <el-descriptions :column="1" border class="purge-preview">
            <el-descriptions-item label="数据域">{{ purgePreview.domain }}</el-descriptions-item>
            <el-descriptions-item label="将影响">
              {{ purgePreview.preview.affectedDocs }} 条文档 · {{ formatBytes(purgePreview.preview.affectedBytes) }}
              （集合：{{ purgePreview.preview.collections.join('、') || '-' }}）
            </el-descriptions-item>
            <el-descriptions-item label="K 副本">
              <el-tag :type="purgeReplicaSufficient ? 'success' : 'danger'">
                {{ purgePreview.replica ? `副本 ${purgePreview.replica.syncedPeers}/${purgePreview.replica.replicaTarget}` : '无法获取（P2P 未启动）' }}
              </el-tag>
            </el-descriptions-item>
          </el-descriptions>
          <el-alert
            v-if="!purgeReplicaSufficient"
            title="副本不足：此时清理本机副本可能造成组织数据丢失，已禁止执行。请等待成员同步补足副本，或改为增加磁盘空间。"
            type="error"
            :closable="false"
            show-icon
            class="purge-tip"
          />
          <el-checkbox v-model="purgeConfirmExported" class="purge-tip">
            我已导出备份并妥善转移，确认清理
          </el-checkbox>
        </template>
      </template>
      <template v-else>
        <el-alert title="清理完成" type="success" :closable="false" show-icon />
        <p class="hint">
          已清理 {{ purgeResult.removedDocs }} 条文档，释放 {{ formatBytes(purgeResult.freedBytes) }}；
          同时代数据不会再同步回本机。
        </p>
      </template>
      <template #footer>
        <template v-if="!purgeResult">
          <el-button @click="purgeDialogVisible = false">取消</el-button>
          <el-button type="danger" :loading="purging" :disabled="!purgeExecutable" @click="executePurge">
            {{ purging ? '清理中...' : '执行清理' }}
          </el-button>
        </template>
        <el-button v-else @click="purgeDialogVisible = false">完成</el-button>
      </template>
    </el-dialog>

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

        <h3 class="section-title">成员列表</h3>
        <el-table :data="selectedOrganization.members" stripe>
          <el-table-column prop="rootId" label="RootID" min-width="240" />
          <el-table-column label="角色" width="100">
            <template #default="scope">
              <el-tag :type="scope.row.role === 'admin' ? 'danger' : 'info'">
                {{ scope.row.role === 'admin' ? '管理员' : '成员' }}
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column label="加入时间" min-width="150">
            <template #default="scope">{{ formatDate(scope.row.joinedAt) }}</template>
          </el-table-column>
          <el-table-column label="PeerId" min-width="200">
            <template #default="scope">{{ scope.row.nodeInfo?.peerId || '-' }}</template>
          </el-table-column>
          <el-table-column label="最近同步" min-width="140">
            <template #default="scope">{{ memberSyncLabel(scope.row.rootId) }}</template>
          </el-table-column>
          <el-table-column v-if="selectedOrganization.isCurrentUserAdmin" label="操作" width="90" fixed="right">
            <template #default="scope">
              <el-button
                v-if="scope.row.rootId !== currentRootId"
                text
                type="danger"
                size="small"
                :loading="removingRootId === scope.row.rootId"
                @click="removeMember(scope.row)"
              >
                移除
              </el-button>
            </template>
          </el-table-column>
        </el-table>

        <div v-if="selectedOrganization.isCurrentUserAdmin" class="drawer-actions">
          <el-button type="primary" @click="openInviteDialog">邀请成员</el-button>
          <el-button v-if="selectedOrganization.basePluginDomain" @click="openOrgPlugin(selectedOrganization)">
            打开插件
          </el-button>
          <el-button v-if="selectedOrganization.basePluginDomain" @click="openPurgeDialog">数据治理</el-button>
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
import { formatBytes } from '../utils/format';

type OrganizationMember = {
  rootId: string;
  role: 'admin' | 'member';
  joinedAt: number;
  addedBy: string;
  nodeInfo?: {
    peerId?: string;
    addresses: string[];
  };
};

type OrganizationView = {
  orgId: string;
  name: string;
  description: string;
  basePluginDomain?: string;
  createdAt: number;
  createdBy: string;
  updatedAt: number;
  members: OrganizationMember[];
  currentUserRole: 'admin' | 'member' | null;
  isCurrentUserAdmin: boolean;
  memberCount: number;
  adminCount: number;
};

type CreateForm = {
  name: string;
  description: string;
  basePluginDomain: string;
};

type OrgSyncOverview = {
  orgId: string;
  replicaTarget: number;
  syncedPeers: number;
  totalMembers: number;
  members: Array<{
    rootId: string;
    peerId?: string;
    isSelf: boolean;
    everSynced: boolean;
    lastSyncedAt: number | null;
  }>;
};

type PurgePreviewResult = {
  domain: string;
  beforeTs: number;
  preview: { collections: string[]; affectedDocs: number; affectedBytes: number };
  replica: OrgSyncOverview | null;
  isCurrentUserAdmin: boolean;
};

type PurgeExecuteResult = {
  removedDocs: number;
  freedBytes: number;
};

type PluginCatalogItem = {
  id: string;
  domain: string;
  name: string;
  description: string;
  category: 'foundation' | 'business';
  version: string;
  views: string[];
};

export default defineComponent({
  name: 'OrgPage',
  emits: ['open-plugin-tab'],
  setup(_, { emit }) {
    const organizations = ref<OrganizationView[]>([]);
    const overviews = ref<Record<string, OrgSyncOverview | null>>({});
    const selectedOrgId = ref('');
    const currentRootId = ref('');
    const loading = ref(false);

    const createDialogVisible = ref(false);
    const joinDialogVisible = ref(false);
    const inviteDialogVisible = ref(false);
    const drawerVisible = ref(false);

    const creating = ref(false);
    const joining = ref(false);
    const inviting = ref(false);
    const deleting = ref(false);
    const removingRootId = ref('');

    const pluginCatalog = ref<PluginCatalogItem[]>([]);
    const createForm = ref<CreateForm>({ name: '', description: '', basePluginDomain: '' });
    const joinCode = ref('');
    const inviteRootId = ref('');
    const invitePeerId = ref('');
    const inviteAddresses = ref('');
    const inviteResult = ref('');

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
        if (!createForm.value.basePluginDomain) {
          createForm.value.basePluginDomain = foundationPlugins.value[0]?.domain ?? '';
        }
      } catch (error) {
        ElMessage.error(`加载插件目录失败：${error}`);
      }
    };

    const openDetail = (organization: OrganizationView) => {
      selectedOrgId.value = organization.orgId;
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

    const openCreateDialog = () => {
      createDialogVisible.value = true;
    };

    const createOrganization = async () => {
      if (!createForm.value.name.trim()) {
        ElMessage.warning('请输入组织名称');
        return;
      }
      if (!createForm.value.basePluginDomain) {
        ElMessage.warning('请选择基础插件');
        return;
      }

      creating.value = true;
      try {
        const created = await window.electronAPI.organization.create({
          name: createForm.value.name,
          description: createForm.value.description,
          basePluginDomain: createForm.value.basePluginDomain
        });
        ElMessage.success(`组织已创建：${created.name}`);
        createForm.value = {
          name: '',
          description: '',
          basePluginDomain: createForm.value.basePluginDomain
        };
        createDialogVisible.value = false;
        await refreshOrganizations();
        selectedOrgId.value = created.orgId;
        drawerVisible.value = true;
      } catch (error) {
        ElMessage.error(`创建组织失败：${error}`);
      } finally {
        creating.value = false;
      }
    };

    const openJoinDialog = () => {
      joinCode.value = '';
      joinDialogVisible.value = true;
    };

    const acceptInvite = async () => {
      if (!joinCode.value.trim()) {
        ElMessage.warning('请输入邀请码');
        return;
      }

      joining.value = true;
      try {
        const joined = await window.electronAPI.organization.acceptInvite(joinCode.value.trim());
        ElMessage.success(`已加入组织：${joined.orgName}`);
        joinDialogVisible.value = false;
        joinCode.value = '';
        await refreshOrganizations();
        selectedOrgId.value = joined.orgId;
        drawerVisible.value = true;
      } catch (error) {
        ElMessage.error(`加入组织失败：${error}`);
      } finally {
        joining.value = false;
      }
    };

    const openInviteDialog = () => {
      inviteRootId.value = '';
      invitePeerId.value = '';
      inviteAddresses.value = '';
      inviteResult.value = '';
      inviteDialogVisible.value = true;
    };

    const resetInviteDialog = () => {
      inviteRootId.value = '';
      invitePeerId.value = '';
      inviteAddresses.value = '';
      inviteResult.value = '';
    };

    const addMemberAndInvite = async () => {
      if (!selectedOrganization.value) {
        return;
      }
      if (!inviteRootId.value.trim()) {
        ElMessage.warning('请输入成员 RootID');
        return;
      }

      const addresses = inviteAddresses.value
        .split(/\r?\n|,|;/)
        .map((item) => item.trim())
        .filter((item) => item.length > 0);
      const nodeInfo = invitePeerId.value.trim() || addresses.length > 0
        ? {
            peerId: invitePeerId.value.trim() || undefined,
            addresses
          }
        : undefined;

      inviting.value = true;
      try {
        await window.electronAPI.organization.addMember(selectedOrganization.value.orgId, {
          rootId: inviteRootId.value,
          nodeInfo
        });
        const invite = await window.electronAPI.organization.createInvite(selectedOrganization.value.orgId);
        inviteResult.value = invite.invite;
        ElMessage.success('成员已预录入');
        await refreshOrganizations();
      } catch (error) {
        ElMessage.error(`邀请成员失败：${error}`);
      } finally {
        inviting.value = false;
      }
    };

    const copyInvite = async () => {
      try {
        await navigator.clipboard.writeText(inviteResult.value);
        ElMessage.success('邀请码已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    // ------------------------------------------------------------------
    // 数据治理：管理员手动清理本机旧数据（先导出转移，K 副本充足才允许执行）
    // ------------------------------------------------------------------
    const purgeDialogVisible = ref(false);
    const purgeBeforeDate = ref<Date | null>(null);
    const purgePreviewing = ref(false);
    const purgeExporting = ref(false);
    const purging = ref(false);
    const purgeConfirmExported = ref(false);
    const purgePreview = ref<PurgePreviewResult | null>(null);
    const purgeResult = ref<PurgeExecuteResult | null>(null);

    const purgeReplicaSufficient = computed(() => {
      const replica = purgePreview.value?.replica;
      return Boolean(replica) && replica!.syncedPeers >= replica!.replicaTarget;
    });

    const purgeExecutable = computed(() => {
      return Boolean(
        purgePreview.value &&
          // 服务端 execute 仍会校验管理员身份；这里用 preview 返回的身份前置禁用，纵深防御
          purgePreview.value.isCurrentUserAdmin &&
          purgePreview.value.preview.affectedDocs > 0 &&
          purgeReplicaSufficient.value &&
          purgeConfirmExported.value
      );
    });

    const resetPurgeDialog = () => {
      purgeBeforeDate.value = null;
      purgePreview.value = null;
      purgeResult.value = null;
      purgeConfirmExported.value = false;
      purgePreviewing.value = false;
      purgeExporting.value = false;
      purging.value = false;
    };

    const openPurgeDialog = () => {
      resetPurgeDialog();
      purgeDialogVisible.value = true;
    };

    const disableFutureDate = (date: Date) => {
      return date.getTime() > Date.now();
    };

    /** 选中日期的 00:00（本地时区）作为清理水位：清理该日期之前的数据 */
    const purgeBeforeTs = () => {
      if (!purgeBeforeDate.value) {
        return 0;
      }
      const date = new Date(purgeBeforeDate.value);
      date.setHours(0, 0, 0, 0);
      return date.getTime();
    };

    const runPurgePreview = async () => {
      if (!selectedOrganization.value || !purgeBeforeDate.value) {
        return;
      }
      purgePreviewing.value = true;
      purgePreview.value = null;
      try {
        purgePreview.value = await window.electronAPI.dataManagement.purgePreview(
          selectedOrganization.value.orgId,
          purgeBeforeTs()
        );
      } catch (error) {
        ElMessage.error(`预览失败：${error}`);
      } finally {
        purgePreviewing.value = false;
      }
    };

    const exportBeforePurge = async () => {
      purgeExporting.value = true;
      try {
        const result = await window.electronAPI.dataManagement.exportData();
        if (result.cancelled) {
          ElMessage.info('已取消导出');
        } else {
          purgeConfirmExported.value = true;
          ElMessage.success(`已导出 ${result.entries} 条数据到 ${result.path}`);
        }
      } catch (error) {
        ElMessage.error(`导出失败：${error}`);
      } finally {
        purgeExporting.value = false;
      }
    };

    const executePurge = async () => {
      if (!selectedOrganization.value || !purgePreview.value) {
        return;
      }
      purging.value = true;
      try {
        purgeResult.value = await window.electronAPI.dataManagement.purgeExecute(
          selectedOrganization.value.orgId,
          purgePreview.value.beforeTs,
          purgeConfirmExported.value
        );
        ElMessage.success('清理完成');
      } catch (error) {
        ElMessage.error(`清理失败：${error}`);
      } finally {
        purging.value = false;
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
      overviews,
      overviewOf,
      selectedOrgId,
      selectedOrganization,
      currentOverview,
      currentRootId,
      loading,
      createDialogVisible,
      joinDialogVisible,
      inviteDialogVisible,
      drawerVisible,
      creating,
      joining,
      inviting,
      deleting,
      removingRootId,
      foundationPlugins,
      adminOrgCount,
      createForm,
      joinCode,
      inviteRootId,
      invitePeerId,
      inviteAddresses,
      inviteResult,
      replicaLabel,
      replicaTagType,
      refreshOrganizations,
      openDetail,
      openOrgPlugin,
      openCreateDialog,
      createOrganization,
      openJoinDialog,
      acceptInvite,
      openInviteDialog,
      resetInviteDialog,
      addMemberAndInvite,
      copyInvite,
      removeMember,
      deleteOrganization,
      memberSyncLabel,
      formatDate,
      purgeDialogVisible,
      purgeBeforeDate,
      purgePreviewing,
      purgeExporting,
      purging,
      purgeConfirmExported,
      purgePreview,
      purgeResult,
      purgeReplicaSufficient,
      purgeExecutable,
      openPurgeDialog,
      resetPurgeDialog,
      disableFutureDate,
      runPurgePreview,
      exportBeforePurge,
      executePurge,
      formatBytes
    };
  }
});
</script>

<style scoped src="../styles/pages/org.css"></style>
