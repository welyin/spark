<!-- 组织信息面板（移植壳层 OrgSettingsPanel「组织信息」卡片）：
     字段逐行展示 + 管理员内联编辑（logo/名称/描述）+ 副本健康度 +
     履职网关 + 成员副本 + 退出组织。数据写面走 sdk.org（桥） -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>组织信息</h2>
    </template>
    <div class="org-info-rows">
      <div class="org-info-row">
        <span class="org-info-label">组织 logo</span>
        <div v-if="editingInfo === 'logo'" class="org-info-edit">
          <AvatarPicker v-model="editInfoValue" :nickname="org.name" :seed="org.orgId" :size="48" />
          <el-button size="small" type="primary" :loading="saving" @click="saveInfo">保存</el-button>
          <el-button size="small" @click="editingInfo = ''">取消</el-button>
        </div>
        <span
          v-else
          class="org-info-value"
          :class="{ editable: org.isCurrentUserAdmin }"
          :title="org.isCurrentUserAdmin ? '点击编辑' : ''"
          @click="startEdit('logo')"
        >
          <OrgAvatar :org-id="org.orgId" :name="org.name" :avatar="org.avatar ?? ''" :size="32" />
        </span>
      </div>
      <div class="org-info-row">
        <span class="org-info-label">名称</span>
        <div v-if="editingInfo === 'name'" class="org-info-edit">
          <el-input v-model="editInfoValue" size="small" maxlength="60" @keyup.enter="saveInfo" />
          <el-button size="small" type="primary" :loading="saving" @click="saveInfo">保存</el-button>
          <el-button size="small" @click="editingInfo = ''">取消</el-button>
        </div>
        <span
          v-else
          class="org-info-value"
          :class="{ editable: org.isCurrentUserAdmin }"
          @click="startEdit('name')"
        >{{ org.name }}</span>
      </div>
      <div class="org-info-row">
        <span class="org-info-label">描述</span>
        <div v-if="editingInfo === 'description'" class="org-info-edit">
          <el-input v-model="editInfoValue" size="small" maxlength="200" @keyup.enter="saveInfo" />
          <el-button size="small" type="primary" :loading="saving" @click="saveInfo">保存</el-button>
          <el-button size="small" @click="editingInfo = ''">取消</el-button>
        </div>
        <span
          v-else
          class="org-info-value"
          :class="{ editable: org.isCurrentUserAdmin }"
          @click="startEdit('description')"
        >{{ org.description || '暂无描述' }}</span>
      </div>
      <div class="org-info-row">
        <span class="org-info-label">管理人员</span>
        <span class="org-info-value">{{ org.adminCount }} 人</span>
      </div>
      <div class="org-info-row">
        <span class="org-info-label">成员</span>
        <span class="org-info-value">{{ org.memberCount }} 人</span>
      </div>
      <div class="org-info-row">
        <span class="org-info-label">最近更新</span>
        <span class="org-info-value">{{ formatDate(org.updatedAt) }}</span>
      </div>
    </div>

    <!-- 副本健康度：kApplicable=false = 纯 all-members 组织无 K，只信息呈现不判定 -->
    <div v-if="overview" class="replica-row">
      <template v-if="overview.kApplicable">
        <el-tag :type="replicaTagType(overview)">{{ replicaLabel(overview) }}</el-tag>
        <span class="replica-hint">
          {{ overview.syncedPeers >= overview.replicaTarget ? '副本充足' : '副本暂不足：待其他成员上线补齐副本，无需额外操作' }}
          （已同步节点 {{ overview.syncedPeers }} / 成员 {{ overview.totalMembers }}）
        </span>
      </template>
      <template v-else>
        <el-tag type="info">全员持有</el-tag>
        <span class="replica-hint">
          本组织数据全员持有，无副本目标（已同步节点 {{ overview.syncedPeers }} / 成员 {{ overview.totalMembers }}）
        </span>
      </template>
    </div>

    <!-- 当前履职网关（只读；全员候选计分推导自动轮换，无设置入口） -->
    <div class="replica-row">
      <span class="replica-hint">当前履职网关：</span>
      <template v-if="gatewayActiveSet.length">
        <el-tag
          v-for="rootId in gatewayActiveSet"
          :key="rootId"
          size="small"
          type="info"
          class="data-account-tag"
        >{{ shortRootId(rootId) }}</el-tag>
      </template>
      <span v-else class="replica-hint replica-hint-warn">
        暂无（组织缺 PC 类成员时为空，建议常备一台常开桌面设备）
      </span>
      <span class="replica-hint">全员候选按在线与活跃自动推导，每小时轮换，无需设置</span>
    </div>

    <!-- 全员数据节点：逐成员 PC 副本状态（信息性提醒，无自动处置） -->
    <div v-if="overview && overview.memberReplicas && overview.memberReplicas.length" class="replica-row">
      <span class="replica-hint">成员副本：</span>
      <el-tag
        v-for="account in overview.memberReplicas"
        :key="account.rootId"
        :type="account.pcSynced ? 'success' : 'warning'"
        size="small"
        class="data-account-tag"
      >{{ shortRootId(account.rootId) }}{{ account.deviceClass === 'mobile' ? '（手机）' : '' }}</el-tag>
      <span v-if="!memberReplicasOk(overview)" class="replica-hint replica-hint-warn">
        成员 PC 副本合计不足 3 份，建议成员常备桌面端在线
      </span>
    </div>

    <!-- 退出组织（删除通路已移除——域只可退出，不可解散） -->
    <div class="org-actions">
      <el-button type="danger" plain :loading="leaving" @click="leaveOrganization">
        {{ leaving ? '退出中...' : '退出组织' }}
      </el-button>
    </div>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginOrgSyncOverview, PluginOrgView } from '../../../../packages/plugin-sdk/src';
import { orgApi } from '../sdk-host';
import {
  formatDate,
  memberReplicasOk,
  networkDegraded,
  replicaLabel,
  replicaTagType,
  shortRootId
} from '../org-store';
import OrgAvatar from './OrgAvatar.vue';
import AvatarPicker from './AvatarPicker.vue';

export default defineComponent({
  name: 'OrgInfoPanel',
  components: { OrgAvatar, AvatarPicker },
  props: {
    org: { type: Object as PropType<PluginOrgView>, required: true },
    overview: { type: Object as PropType<PluginOrgSyncOverview | null>, default: null },
    gatewayActiveSet: { type: Array as PropType<string[]>, default: () => [] }
  },
  emits: ['changed'],
  setup(props, { emit }) {
    const editingInfo = ref<'' | 'logo' | 'name' | 'description'>('');
    const editInfoValue = ref('');
    const saving = ref(false);
    const leaving = ref(false);

    const startEdit = (field: 'logo' | 'name' | 'description') => {
      if (!props.org.isCurrentUserAdmin) {
        return;
      }
      editingInfo.value = field;
      editInfoValue.value =
        field === 'logo' ? props.org.avatar ?? '' : field === 'name' ? props.org.name : props.org.description;
    };

    /** 写操作前网络检查：组织网络丢失/仅本地时提示（只提示，不阻断） */
    const notifyIfNetworkUnavailable = () => {
      if (networkDegraded(props.overview)) {
        ElMessage.warning('当前组织网络不可用，数据将在恢复后同步');
      }
    };

    const saveInfo = async () => {
      const api = orgApi();
      if (!api || !editingInfo.value) {
        return;
      }
      const field = editingInfo.value;
      const value = field === 'logo' ? editInfoValue.value : editInfoValue.value.trim();
      if (field === 'name' && !value) {
        ElMessage.warning('名称不能为空');
        return;
      }
      saving.value = true;
      try {
        notifyIfNetworkUnavailable();
        // avatar 空串 = 清除 logo（内核 settings.rs 口径）；logo 持久化在内核
        // （随组织同步），不再写壳层 localStorage 展示缓存
        await api.updateInfo(props.org.orgId, { [field]: value });
        editingInfo.value = '';
        ElMessage.success('已保存');
        emit('changed');
      } catch (error) {
        ElMessage.error(`保存失败：${error}`);
      } finally {
        saving.value = false;
      }
    };

    // 退出组织：留史语义——组织历史原样保留，本机组织数据转为只读档案
    const leaveOrganization = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      const isLastMember = props.org.memberCount <= 1;
      const message = isLastMember
        ? `你是「${props.org.name}」最后一名成员，退出后组织即成为空域：历史保留为只读档案，无人能再写入。确认退出？`
        : `确认退出组织「${props.org.name}」？退出后组织历史原样保留，本机上的组织数据转为只读档案。`;
      try {
        await ElMessageBox.confirm(message, '退出确认', {
          type: 'warning',
          confirmButtonText: '确认退出',
          cancelButtonText: '取消'
        });
      } catch {
        return;
      }
      leaving.value = true;
      try {
        await api.leave(props.org.orgId);
        ElMessage.success('已退出组织');
        emit('changed');
      } catch (error) {
        ElMessage.error(`退出组织失败：${error}`);
      } finally {
        leaving.value = false;
      }
    };

    return {
      editingInfo,
      editInfoValue,
      saving,
      leaving,
      startEdit,
      saveInfo,
      leaveOrganization,
      formatDate,
      shortRootId,
      replicaLabel,
      replicaTagType,
      memberReplicasOk
    };
  }
});
</script>

<style scoped>
.panel-card h2 {
  margin: 0;
  font-size: 16px;
}

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
}

.org-info-value {
  min-width: 0;
  text-align: right;
  word-break: break-all;
}

.org-info-value.editable {
  cursor: pointer;
}

.org-info-value.editable:hover {
  color: var(--spark-primary);
}

.org-info-edit {
  display: flex;
  flex: 1;
  min-width: 0;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
}

.org-info-edit .el-input {
  max-width: 280px;
}

.replica-row {
  margin-top: 12px;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
}

.replica-hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.replica-hint-warn {
  color: var(--el-color-warning);
}

.data-account-tag {
  margin-right: 4px;
}

.org-actions {
  margin-top: 16px;
  display: flex;
  gap: 8px;
}
</style>
