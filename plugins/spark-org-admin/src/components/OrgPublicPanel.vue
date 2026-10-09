<!-- 公开设置面板（移植壳层 PublicOrgPanel）：开关 + 展示名编辑 + 组织地址展示复制 -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>公开设置</h2>
    </template>
    <p class="hint">
      公开后，其他 Spark 用户可通过组织地址在 DHT 上找到本组织网关。组织根密钥仅保存在创建者本机，不会同步。
    </p>
    <div v-if="org.orgAddress" class="org-address-row">
      <code class="org-address-code">{{ org.orgAddress }}</code>
      <el-button text type="primary" size="small" @click="copyOrgAddress">复制地址</el-button>
    </div>
    <p v-else class="hint">本组织还没有组织地址（早期版本创建），开启公开后将自动生成。</p>
    <div v-if="org.isCurrentUserAdmin" class="public-editor">
      <div class="public-switch-row">
        <el-switch v-model="enabled" />
        <span>{{ enabled ? '已公开' : '未公开' }}</span>
      </div>
      <el-input v-model="displayName" placeholder="展示名（可选，缺省用组织名）" class="public-name-input" />
      <el-button type="primary" size="small" :loading="saving" class="public-save" @click="save">
        保存公开设置
      </el-button>
    </div>
    <p v-else class="hint">{{ org.isPublic ? '本组织已公开。' : '本组织未公开。' }}</p>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import type { PluginOrgView } from '../../../../packages/plugin-sdk/src';
import { orgApi } from '../sdk-host';

export default defineComponent({
  name: 'OrgPublicPanel',
  props: {
    org: { type: Object as PropType<PluginOrgView>, required: true }
  },
  emits: ['changed'],
  setup(props, { emit }) {
    const enabled = ref(props.org.isPublic ?? false);
    const displayName = ref(props.org.orgDisplayName ?? '');
    const saving = ref(false);

    // 组织切换后重置本地编辑态
    watch(
      () => props.org.orgId,
      () => {
        enabled.value = props.org.isPublic ?? false;
        displayName.value = props.org.orgDisplayName ?? '';
      }
    );

    const copyOrgAddress = async () => {
      try {
        await navigator.clipboard.writeText(props.org.orgAddress!);
        ElMessage.success('组织地址已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const save = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      saving.value = true;
      try {
        await api.setPublic(props.org.orgId, enabled.value, displayName.value.trim() || undefined);
        ElMessage.success(enabled.value ? '组织已公开' : '组织已取消公开');
        emit('changed');
      } catch (error) {
        ElMessage.error(`保存公开设置失败：${error}`);
      } finally {
        saving.value = false;
      }
    };

    return { enabled, displayName, saving, copyOrgAddress, save };
  }
});
</script>

<style scoped>
.panel-card h2 {
  margin: 0;
  font-size: 16px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.org-address-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 8px 0;
}

.org-address-code {
  word-break: break-all;
  font-size: 12px;
  color: var(--spark-text-2);
}

.public-editor {
  margin-top: 12px;
}

.public-switch-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.public-name-input {
  margin-top: 8px;
}

.public-save {
  margin-top: 8px;
}
</style>
