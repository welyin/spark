<!-- 职责：组织详情抽屉内的「组织网关」区块——展示当前网关并提供管理员编辑入口 -->
<template>
  <h3 class="section-title">组织网关</h3>
  <p class="hint">
    网关节点负责在组织私有 DHT 上发布成员线索，帮助失联成员找回组织。需指定 2-3 名本组织成员。
  </p>
  <div v-if="gateways.length" class="gateway-list">
    <el-tag v-for="rootId in gateways" :key="rootId" class="gateway-tag" type="success">
      {{ gatewayLabel(rootId) }}
    </el-tag>
  </div>
  <p v-else class="hint">尚未指定网关节点。</p>
  <div v-if="isAdmin" class="gateway-editor">
    <el-select
      v-model="selection"
      multiple
      :multiple-limit="3"
      placeholder="选择 2-3 名成员作为网关"
      style="width: 100%"
    >
      <el-option
        v-for="member in members"
        :key="member.rootId"
        :label="memberLabel(member)"
        :value="member.rootId"
      />
    </el-select>
    <el-button
      type="primary"
      size="small"
      :loading="saving"
      :disabled="!valid"
      style="margin-top: 8px"
      @click="onSave"
    >
      保存网关设置
    </el-button>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import type { OrganizationMember } from './types';

export default defineComponent({
  name: 'GatewayManager',
  props: {
    // 当前选中的网关 rootId 列表（v-model）
    modelValue: { type: Array as PropType<string[]>, required: true },
    members: { type: Array as PropType<OrganizationMember[]>, required: true },
    gateways: { type: Array as PropType<string[]>, required: true },
    isAdmin: { type: Boolean, required: true },
    saving: { type: Boolean, required: true },
    valid: { type: Boolean, required: true }
  },
  emits: ['update:modelValue', 'save'],
  setup(props, { emit }) {
    const selection = computed({
      get: () => props.modelValue,
      set: (value: string[]) => emit('update:modelValue', value)
    });

    const memberLabel = (member: OrganizationMember) => {
      const role = member.role === 'admin' ? '管理员' : '成员';
      return `${member.rootId.slice(0, 12)}...（${role}）`;
    };

    const gatewayLabel = (rootId: string) => {
      const member = props.members.find((item) => item.rootId === rootId);
      return member ? memberLabel(member) : `${rootId.slice(0, 12)}...`;
    };

    const onSave = () => {
      emit('save');
    };

    return {
      selection,
      memberLabel,
      gatewayLabel,
      onSave
    };
  }
});
</script>
