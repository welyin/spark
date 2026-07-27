<!-- 职责：数据治理对话框——管理员手动清理本机旧数据（先导出转移，K 副本充足才允许执行） -->
<template>
  <el-dialog v-model="dialogVisible" title="数据治理（清理本机旧数据）" width="560px" @closed="resetPurgeDialog">
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
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="danger" :loading="purging" :disabled="!purgeExecutable" @click="executePurge">
          {{ purging ? '清理中...' : '执行清理' }}
        </el-button>
      </template>
      <el-button v-else @click="dialogVisible = false">完成</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { formatBytes } from '../../utils/format';
import type { OrgSyncOverviewDto as OrgSyncOverview } from '../../api';

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

export default defineComponent({
  name: 'PurgeDataDialog',
  props: {
    // 对话框可见性（v-model）
    modelValue: { type: Boolean, required: true },
    orgId: { type: String, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    // ------------------------------------------------------------------
    // 数据治理：管理员手动清理本机旧数据（先导出转移，K 副本充足才允许执行）
    // ------------------------------------------------------------------
    const purgeBeforeDate = ref<Date | null>(null);
    const purgePreviewing = ref(false);
    const purgeExporting = ref(false);
    const purging = ref(false);
    const purgeConfirmExported = ref(false);
    const purgePreview = ref<PurgePreviewResult | null>(null);
    const purgeResult = ref<PurgeExecuteResult | null>(null);

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

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

    // 打开对话框时重置状态（对齐原 openPurgeDialog 行为）
    watch(
      () => props.modelValue,
      (visible) => {
        if (visible) {
          resetPurgeDialog();
        }
      }
    );

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
      if (!props.orgId || !purgeBeforeDate.value) {
        return;
      }
      purgePreviewing.value = true;
      purgePreview.value = null;
      try {
        purgePreview.value = await window.electronAPI.dataManagement.purgePreview(
          props.orgId,
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
      if (!props.orgId || !purgePreview.value) {
        return;
      }
      purging.value = true;
      try {
        purgeResult.value = await window.electronAPI.dataManagement.purgeExecute(
          props.orgId,
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

    return {
      dialogVisible,
      purgeBeforeDate,
      purgePreviewing,
      purgeExporting,
      purging,
      purgeConfirmExported,
      purgePreview,
      purgeResult,
      purgeReplicaSufficient,
      purgeExecutable,
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
