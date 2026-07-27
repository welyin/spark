<!-- 职责：组织详情抽屉内的成员列表表格（含管理员移除成员操作） -->
<template>
  <h3 class="section-title">成员列表</h3>
  <el-table :data="members" stripe>
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
    <el-table-column v-if="isAdmin" label="操作" width="90" fixed="right">
      <template #default="scope">
        <el-button
          v-if="scope.row.rootId !== currentRootId"
          text
          type="danger"
          size="small"
          :loading="removingRootId === scope.row.rootId"
          @click="onRemove(scope.row)"
        >
          移除
        </el-button>
      </template>
    </el-table-column>
  </el-table>
</template>

<script lang="ts">
import { defineComponent, type PropType } from 'vue';
import type { OrganizationMember } from './types';

export default defineComponent({
  name: 'MemberList',
  props: {
    members: { type: Array as PropType<OrganizationMember[]>, required: true },
    isAdmin: { type: Boolean, required: true },
    currentRootId: { type: String, required: true },
    removingRootId: { type: String, required: true },
    memberSyncLabel: { type: Function as PropType<(rootId: string) => string>, required: true },
    formatDate: { type: Function as PropType<(timestamp: number) => string>, required: true }
  },
  emits: ['remove'],
  setup(_, { emit }) {
    const onRemove = (member: OrganizationMember) => {
      emit('remove', member);
    };

    return {
      onRemove
    };
  }
});
</script>
