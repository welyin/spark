<!-- 职责：组织详情抽屉内的「公开组织」区块——开关 + 展示名编辑 + 组织地址展示复制 -->
<template>
  <h3 class="section-title">公开组织</h3>
  <p class="hint">
    公开后，其他 Spark 用户可通过组织地址在 DHT 上找到本组织网关。组织根密钥仅保存在创建者本机，不会同步。
  </p>
  <div v-if="org.orgAddress" class="org-address-row">
    <code class="org-address-code">{{ org.orgAddress }}</code>
    <el-button text type="primary" size="small" @click="copyOrgAddress(org.orgAddress!)">
      复制地址
    </el-button>
  </div>
  <p v-else class="hint">本组织还没有组织地址（早期版本创建），开启公开后将自动生成。</p>
  <div v-if="org.isCurrentUserAdmin" class="public-editor">
    <div class="public-switch-row">
      <el-switch v-model="enabled" />
      <span>{{ enabled ? '已公开' : '未公开' }}</span>
    </div>
    <el-input
      v-model="displayName"
      placeholder="展示名（可选，缺省用组织名）"
      style="margin-top: 8px"
    />
    <el-button
      type="primary"
      size="small"
      :loading="saving"
      style="margin-top: 8px"
      @click="onSave"
    >
      保存公开设置
    </el-button>
  </div>
  <p v-else class="hint">
    {{ org.isPublic ? '本组织已公开。' : '本组织未公开。' }}
  </p>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import type { OrganizationView } from './types';

export default defineComponent({
  name: 'PublicOrgPanel',
  props: {
    org: { type: Object as PropType<OrganizationView>, required: true },
    // 公开开关（v-model:enabled）
    enabled: { type: Boolean, required: true },
    // 展示名（v-model:displayName）
    displayName: { type: String, required: true },
    saving: { type: Boolean, required: true }
  },
  emits: ['update:enabled', 'update:displayName', 'save'],
  setup(props, { emit }) {
    const enabled = computed({
      get: () => props.enabled,
      set: (value: boolean) => emit('update:enabled', value)
    });

    const displayName = computed({
      get: () => props.displayName,
      set: (value: string) => emit('update:displayName', value)
    });

    const copyOrgAddress = async (orgAddress: string) => {
      try {
        await navigator.clipboard.writeText(orgAddress);
        ElMessage.success('组织地址已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const onSave = () => {
      emit('save');
    };

    return {
      enabled,
      displayName,
      copyOrgAddress,
      onSave
    };
  }
});
</script>
