<!-- 成员名册面板（A42 名册管理）：成员列表（角色/加入时间/「我」高亮）+
     管理员操作（名片预录+定向邀请、移除成员、邀请码生成、邀请记录）。
     数据面走 sdk.org（桥；orgId 作用域由桥按绑定空间强制） -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>成员名册</h2>
    </template>

    <div v-if="org.isCurrentUserAdmin" class="roster-actions">
      <el-button size="small" type="primary" @click="inviteDialogVisible = true">添加成员</el-button>
      <el-button size="small" :loading="makingInvite" @click="createInviteCode">生成邀请码</el-button>
    </div>

    <!-- 邀请码（24 小时有效；配合预录成员使用） -->
    <div v-if="inviteCode" class="invite-code-row">
      <el-input v-model="inviteCode" type="textarea" :rows="3" readonly />
      <el-button text type="primary" size="small" @click="copyText(inviteCode, '邀请码已复制')">复制</el-button>
      <p class="hint">邀请码 24 小时内有效，用于被邀请人连接本节点并拉取组织数据。</p>
    </div>

    <el-table :data="members" class="roster-table" row-key="rootId">
      <el-table-column label="成员" min-width="180">
        <template #default="{ row }">
          <span class="member-name">
            {{ memberDisplayName(row) }}
            <el-tag v-if="row.rootId === selfRootId" size="small" type="success">我</el-tag>
            <el-tag v-if="row.kind === 'org'" size="small" type="info">组织</el-tag>
          </span>
        </template>
      </el-table-column>
      <el-table-column label="角色" width="90">
        <template #default="{ row }">
          <el-tag size="small" :type="row.role === 'admin' ? 'warning' : 'info'">
            {{ row.role === 'admin' ? '管理员' : '成员' }}
          </el-tag>
        </template>
      </el-table-column>
      <el-table-column label="加入时间" width="150">
        <template #default="{ row }">{{ formatDate(row.joinedAt) }}</template>
      </el-table-column>
      <el-table-column v-if="org.isCurrentUserAdmin" label="操作" width="90">
        <template #default="{ row }">
          <el-button
            v-if="row.rootId !== selfRootId"
            text
            type="danger"
            size="small"
            :loading="removingRootId === row.rootId"
            @click="removeMember(row)"
          >移除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <!-- 邀请记录（出/入站合并；轻量列表） -->
    <template v-if="inviteRecords.length">
      <h3 class="section-title">邀请记录</h3>
      <div v-for="record in inviteRecords" :key="record.id" class="invite-record-row">
        <el-tag size="small" :type="record.direction === 'outgoing' ? 'info' : 'primary'">
          {{ record.direction === 'outgoing' ? '发出' : '收到' }}
        </el-tag>
        <span class="invite-record-name">{{ record.peerNickname || shortRootId(record.peerRootId) }}</span>
        <el-tag
          size="small"
          :type="record.status === 'accepted' ? 'success' : record.status === 'declined' ? 'danger' : 'warning'"
        >{{ record.status === 'accepted' ? '已接受' : record.status === 'declined' ? '已拒绝' : '待确认' }}</el-tag>
        <span class="hint">{{ formatDate(record.updatedAt) }}</span>
      </div>
    </template>

    <!-- 添加成员对话框：名片输入 → 预录成员 + 定向 DM 邀请（移植 InviteMemberDialog 主流程；
         壳层 mock 面（仅本地拦截/出站记录即合入）不迁移，见任务报告） -->
    <el-dialog v-model="inviteDialogVisible" title="添加成员" width="480px" @closed="cardRaw = ''">
      <p class="hint">让对方打开「个人设置 → 我的名片」，把名片内容发给你（可识别 JSON 名片或含 RootID 的文本）。</p>
      <el-input
        v-model="cardRaw"
        type="textarea"
        :rows="5"
        placeholder="粘贴对方的名片内容"
      />
      <template #footer>
        <el-button @click="inviteDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="inviting" @click="addMemberAndInvite">
          {{ inviting ? '处理中...' : '添加并发送邀请' }}
        </el-button>
      </template>
    </el-dialog>
  </el-card>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginOrgInviteRecord, PluginOrgMember, PluginOrgView } from '../../../../packages/plugin-sdk/src';
import { currentRootId, orgApi } from '../sdk-host';
import { formatDate, memberDisplayName, parseMemberCard, shortRootId, sortedMembers } from '../org-store';

export default defineComponent({
  name: 'OrgRosterPanel',
  props: {
    org: { type: Object as PropType<PluginOrgView>, required: true }
  },
  emits: ['changed'],
  setup(props, { emit }) {
    const selfRootId = ref('');
    const inviteDialogVisible = ref(false);
    const cardRaw = ref('');
    const inviting = ref(false);
    const makingInvite = ref(false);
    const inviteCode = ref('');
    const removingRootId = ref('');
    const inviteRecords = ref<PluginOrgInviteRecord[]>([]);

    const members = computed(() => sortedMembers(props.org.members));

    const loadInviteRecords = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      try {
        inviteRecords.value = await api.inviteRecords(props.org.orgId);
      } catch {
        inviteRecords.value = [];
      }
    };

    onMounted(async () => {
      selfRootId.value = await currentRootId();
      await loadInviteRecords();
    });

    const copyText = async (text: string, message: string) => {
      try {
        await navigator.clipboard.writeText(text);
        ElMessage.success(message);
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const createInviteCode = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      makingInvite.value = true;
      try {
        const result = await api.createInvite(props.org.orgId);
        inviteCode.value = result.invite;
      } catch (error) {
        ElMessage.error(`生成邀请码失败：${error}`);
      } finally {
        makingInvite.value = false;
      }
    };

    const addMemberAndInvite = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      const card = parseMemberCard(cardRaw.value);
      if (!card) {
        ElMessage.warning('未从内容中识别到有效的身份 ID，请确认名片内容完整');
        return;
      }
      const nodeInfo =
        card.peerId || card.addresses?.length
          ? { peerId: card.peerId, addresses: card.addresses ?? [] }
          : undefined;
      inviting.value = true;
      try {
        // 预录成员（rootId + 名片寻址线索，名册即时可见）→ 定向 DM 邀请对方确认
        await api.addMember(props.org.orgId, { rootId: card.rootId, nodeInfo });
        await api.sendInvite({
          orgId: props.org.orgId,
          targetRootId: card.rootId,
          targetPeerId: card.peerId ?? null,
          targetAddresses: card.addresses ?? null
        });
        ElMessage.success('邀请已发送，等待对方确认');
        inviteDialogVisible.value = false;
        await loadInviteRecords();
        emit('changed');
      } catch (error) {
        // 内核错误原文提示（如「无法确定对方节点地址」）
        ElMessage.error(`${error}`);
      } finally {
        inviting.value = false;
      }
    };

    const removeMember = async (member: PluginOrgMember) => {
      const api = orgApi();
      if (!api) {
        return;
      }
      try {
        await ElMessageBox.confirm(
          `确认将成员「${memberDisplayName(member)}」移出组织？对方设备上的组织数据将转为只读档案。`,
          '移除成员',
          { type: 'warning', confirmButtonText: '移除', cancelButtonText: '取消' }
        );
      } catch {
        return;
      }
      removingRootId.value = member.rootId;
      try {
        await api.removeMember(props.org.orgId, member.rootId);
        ElMessage.success('已移除成员');
        emit('changed');
      } catch (error) {
        ElMessage.error(`移除成员失败：${error}`);
      } finally {
        removingRootId.value = '';
      }
    };

    return {
      selfRootId,
      members,
      inviteDialogVisible,
      cardRaw,
      inviting,
      makingInvite,
      inviteCode,
      removingRootId,
      inviteRecords,
      copyText,
      createInviteCode,
      addMemberAndInvite,
      removeMember,
      memberDisplayName,
      shortRootId,
      formatDate
    };
  }
});
</script>

<style scoped>
.panel-card h2 {
  margin: 0;
  font-size: 16px;
}

.section-title {
  margin: 20px 0 8px;
  font-size: 14px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.roster-actions {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}

.invite-code-row {
  margin-bottom: 12px;
}

.roster-table {
  width: 100%;
}

.member-name {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.invite-record-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  border-bottom: 1px solid var(--spark-border-light);
}

.invite-record-row:last-child {
  border-bottom: 0;
}

.invite-record-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
