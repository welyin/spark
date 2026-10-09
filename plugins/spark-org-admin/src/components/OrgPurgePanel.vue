<!-- 数据治理面板（移植壳层 PurgeDataPanel）：管理员手动清理本机旧数据
     （先导出转移，K 副本充足才允许执行）。数据面走 sdk.org 的 purge*/exportData -->
<template>
  <div class="purge-panel">
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
        <el-button :loading="previewing" :disabled="!purgeBeforeDate" @click="runPreview">预览影响</el-button>
        <el-button :loading="exporting" @click="exportBeforePurge">导出数据</el-button>
      </div>
      <template v-if="preview">
        <el-descriptions :column="1" border class="purge-preview">
          <el-descriptions-item label="数据域">{{ preview.domain }}</el-descriptions-item>
          <el-descriptions-item label="将影响">
            {{ preview.preview.affectedDocs }} 条文档 · {{ formatBytes(preview.preview.affectedBytes) }}
            （集合：{{ preview.preview.collections.join('、') || '-' }}）
          </el-descriptions-item>
          <el-descriptions-item label="K 副本">
            <el-tag :type="replicaSufficient ? 'success' : 'danger'">
              {{ preview.replica ? `副本 ${preview.replica.syncedPeers}/${preview.replica.replicaTarget}` : '无法获取（P2P 未启动）' }}
            </el-tag>
          </el-descriptions-item>
        </el-descriptions>
        <el-alert
          v-if="!replicaSufficient"
          title="副本不足：此时清理本机副本可能造成组织数据丢失，已禁止执行。请等待成员同步补足副本，或改为增加磁盘空间。"
          type="error"
          :closable="false"
          show-icon
          class="purge-tip"
        />
        <el-checkbox v-model="confirmExported" class="purge-tip">
          我已导出备份并妥善转移，确认清理
        </el-checkbox>
        <div class="purge-actions">
          <el-button type="danger" :loading="purging" :disabled="!executable" @click="executePurge">
            {{ purging ? '清理中...' : '执行清理' }}
          </el-button>
        </div>
      </template>
    </template>
    <template v-else>
      <el-alert title="清理完成" type="success" :closable="false" show-icon />
      <p class="hint">
        已清理 {{ purgeResult.removedDocs }} 条文档，释放 {{ formatBytes(purgeResult.freedBytes) }}；
        同时代数据不会再同步回本机。
      </p>
      <div class="purge-actions">
        <el-button @click="reset">继续清理</el-button>
      </div>
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import type { PluginOrgPurgePreview, PluginOrgPurgeResult } from '../../../../packages/plugin-sdk/src';
import { orgApi } from '../sdk-host';
import { formatBytes, purgeBeforeTsOf, purgeExecutable, purgeReplicaSufficient } from '../org-store';

export default defineComponent({
  name: 'OrgPurgePanel',
  props: {
    orgId: { type: String, required: true }
  },
  setup(props) {
    const purgeBeforeDate = ref<Date | null>(null);
    const previewing = ref(false);
    const exporting = ref(false);
    const purging = ref(false);
    const confirmExported = ref(false);
    const preview = ref<PluginOrgPurgePreview | null>(null);
    const purgeResult = ref<PluginOrgPurgeResult | null>(null);

    const replicaSufficient = computed(() => purgeReplicaSufficient(preview.value));
    const executable = computed(() => purgeExecutable(preview.value, confirmExported.value));

    const reset = () => {
      purgeBeforeDate.value = null;
      preview.value = null;
      purgeResult.value = null;
      confirmExported.value = false;
      previewing.value = false;
      exporting.value = false;
      purging.value = false;
    };

    // 切换组织时重置状态（对齐壳层面板每次打开重置的行为）
    watch(() => props.orgId, reset);

    const disableFutureDate = (date: Date) => date.getTime() > Date.now();

    const runPreview = async () => {
      const api = orgApi();
      if (!api || !props.orgId || !purgeBeforeDate.value) {
        return;
      }
      previewing.value = true;
      preview.value = null;
      try {
        preview.value = await api.purgePreview(props.orgId, purgeBeforeTsOf(purgeBeforeDate.value));
      } catch (error) {
        ElMessage.error(`预览失败：${error}`);
      } finally {
        previewing.value = false;
      }
    };

    const exportBeforePurge = async () => {
      const api = orgApi();
      if (!api) {
        return;
      }
      exporting.value = true;
      try {
        const result = await api.exportData();
        if (result.cancelled) {
          ElMessage.info('已取消导出');
        } else {
          confirmExported.value = true;
          ElMessage.success(`已导出 ${result.entries} 条数据到 ${result.path}`);
        }
      } catch (error) {
        ElMessage.error(`导出失败：${error}`);
      } finally {
        exporting.value = false;
      }
    };

    const executePurge = async () => {
      const api = orgApi();
      if (!api || !props.orgId || !preview.value) {
        return;
      }
      purging.value = true;
      try {
        purgeResult.value = await api.purgeExecute(props.orgId, preview.value.beforeTs, confirmExported.value);
        ElMessage.success('清理完成');
      } catch (error) {
        ElMessage.error(`清理失败：${error}`);
      } finally {
        purging.value = false;
      }
    };

    return {
      purgeBeforeDate,
      previewing,
      exporting,
      purging,
      confirmExported,
      preview,
      purgeResult,
      replicaSufficient,
      executable,
      reset,
      disableFutureDate,
      runPreview,
      exportBeforePurge,
      executePurge,
      formatBytes
    };
  }
});
</script>

<style scoped>
.purge-tip {
  margin-top: 12px;
}

.purge-actions {
  margin-top: 12px;
  display: flex;
  gap: 8px;
}

.purge-preview {
  margin-top: 12px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}
</style>
