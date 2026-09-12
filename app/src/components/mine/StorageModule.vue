<!-- 存储与副本模块（SettingsPage「个人设置」，A3）：
     副本健康度展示（设备数 / 副本目标 K / blob 副本短板 / 配额水位）+ 配额配置。
     口径（personal-data §4.5/Q06）：副本不足**只提醒不处置**，个人域零机制性干预；
     设备 ≤3 台时 K 退化为全量（副本上限自然等于设备数），展示中如实表达。 -->
<template>
  <div class="mine-list storage-module">
    <h2 class="mine-list-title">存储与副本</h2>
    <!-- M15：手机端如实标注叶子模式——本机只作暂存缓存，不对外提供副本、不承诺 K 副本；
         「哪些设备持有完整副本」入口跳设备管理（逐设备副本角色内核暂无查询接口，如实注明） -->
    <el-alert
      v-if="isMobileLayout"
      type="info"
      :closable="false"
      show-icon
      class="storage-alert"
      title="本机为手机端（叶子模式）：数据仅作本机暂存缓存，不承诺为他人提供副本、不承诺 K 副本；副本承诺由你的 PC / 常在线设备承担。"
    />
    <div v-if="isMobileLayout" class="storage-leaf-link">
      <el-button native-type="button" size="small" @click="emit('open-devices')">
        哪些设备持有完整副本（设备管理）
      </el-button>
      <p class="storage-note">逐设备副本角色内核暂无查询接口，暂以设备列表为准。</p>
    </div>
    <div v-if="health" class="storage-body">
      <el-alert
        v-if="health.underKBlobs > 0"
        :title="`有 ${health.underKBlobs} 个 blob 副本不足 ${health.kTarget} 份——副本不足只提醒不处置，请在设备在线时同步以补足副本`"
        type="warning"
        :closable="false"
        show-icon
        class="storage-alert"
      />
      <div class="storage-card">
        <h3 class="storage-card-title">副本健康度</h3>
        <p class="storage-headline">你当前只有 {{ replicaHeadline }} 份副本</p>
        <ul class="storage-facts">
          <li>
            设备：{{ health.deviceCount }} 台（核心数据全量
            {{ health.deviceCount }} 份副本）
          </li>
          <li>
            副本目标：K = {{ health.kTarget }}（设备不超过 3 台时退化为全量）
          </li>
          <li>
            blob 数据：{{ health.totalBlobs }} 个（{{
              fmtBytes(health.totalBytes)
            }}），
            <template v-if="health.underKBlobs > 0"
              >{{ health.underKBlobs }} 个副本不足 K；</template
            >
            <template v-else>副本均达标；</template>
            <template v-if="health.minFullReplicas !== null"
              >最差副本水位 {{ health.minFullReplicas }} 份</template
            >
          </li>
        </ul>
        <p class="storage-note">
          数据只存于你自己的设备；副本数随设备上下线如实变化，系统不做任何机制性干预。
        </p>
        <!-- G7：个人数据不承诺组织级「永不丢」，引导设备信任与导出备份 -->
        <p class="storage-note">
          个人空间数据为自有设备副本：设备全部丢失即不可恢复，我们不承诺「永不丢」。
          建议保持多台可信设备在线（设备管理），并定期导出备份。
        </p>
        <div class="storage-backup-link">
          <el-button
            native-type="button"
            size="small"
            @click="emit('open-backup')"
            >前往账号备份</el-button
          >
        </div>
      </div>
      <div class="storage-card">
        <h3 class="storage-card-title">存储配额</h3>
        <p class="storage-facts">
          当前水位 {{ fmtBytes(health.quota.usedBytes) }} /
          {{ fmtBytes(health.quota.quotaBytes) }}
          <span v-if="health.quota.overBytes > 0" class="storage-over">
            （已超出
            {{
              fmtBytes(health.quota.overBytes)
            }}；超出副本目标的富余副本将按「最久未访问」自动驱逐，绝不删除最后副本）
          </span>
        </p>
        <div class="storage-quota-form">
          <el-input-number
            v-model="quotaGb"
            :min="1"
            :max="4096"
            :step="1"
            controls-position="right"
          />
          <span class="storage-quota-unit">GB</span>
          <el-button
            type="primary"
            native-type="button"
            :loading="saving"
            @click="saveQuota"
            >保存</el-button
          >
          <el-button native-type="button" :disabled="saving" @click="resetQuota"
            >恢复默认</el-button
          >
        </div>
        <p class="storage-note">
          默认 PC 10 GB / 移动 1 GB；恢复默认即清除自定义配置。
        </p>
      </div>
    </div>
    <el-empty v-else-if="loadError" :description="loadError" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import type { BlobHealthDto } from '../../api/types';
import { errorMessage } from '../../utils/ipc';
import { isMobileLayout } from '../../stores/ui-layout';

const GIB = 1024 * 1024 * 1024;

function fmtBytes(bytes: number): string {
  if (bytes >= GIB) return `${(bytes / GIB).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

type BlobApi = {
  health: () => Promise<BlobHealthDto>;
  getQuota: () => Promise<number>;
  setQuota: (bytes: number | null) => Promise<void>;
};

function blobApi(): BlobApi | null {
  return (
    (window as unknown as { electronAPI?: { blob?: BlobApi } }).electronAPI
      ?.blob ?? null
  );
}

export default defineComponent({
  name: 'StorageModule',
  emits: ['open-backup', 'open-devices'],
  setup(_, { emit }) {
    const health = ref<BlobHealthDto | null>(null);
    const quotaGb = ref(10);
    const saving = ref(false);
    const loadError = ref('');
    const isMobile = isMobileLayout;

    /** 头部副本数：有 blob 取最差副本水位，无 blob 按核心数据口径（= 设备数） */
    const replicaHeadline = computed(() => {
      const h = health.value;
      if (!h) return 1;
      return h.minFullReplicas ?? h.deviceCount;
    });

    async function load() {
      const api = blobApi();
      if (!api) {
        loadError.value = '宿主接口不可用';
        return;
      }
      try {
        const [h, q] = await Promise.all([api.health(), api.getQuota()]);
        health.value = h;
        quotaGb.value = Math.max(1, Math.round(q / GIB));
        loadError.value = '';
      } catch (e) {
        loadError.value = errorMessage(e);
      }
    }

    async function saveQuota() {
      const api = blobApi();
      if (!api) return;
      saving.value = true;
      try {
        await api.setQuota(quotaGb.value * GIB);
        ElMessage.success('配额已保存');
        await load();
      } catch (e) {
        ElMessage.error(errorMessage(e));
      } finally {
        saving.value = false;
      }
    }

    async function resetQuota() {
      const api = blobApi();
      if (!api) return;
      saving.value = true;
      try {
        await api.setQuota(null);
        ElMessage.success('已恢复默认配额');
        await load();
      } catch (e) {
        ElMessage.error(errorMessage(e));
      } finally {
        saving.value = false;
      }
    }

    onMounted(load);

    return {
      health,
      quotaGb,
      saving,
      loadError,
      replicaHeadline,
      fmtBytes,
      saveQuota,
      resetQuota,
      emit,
      isMobileLayout: isMobile,
    };
  },
});
</script>

<style scoped>
.storage-body {
  padding: 12px 16px;
  overflow-y: auto;
}
.storage-alert {
  margin-bottom: 12px;
}
.storage-card {
  background: var(--el-fill-color-light, #f5f7fa);
  border-radius: 8px;
  padding: 14px 16px;
  margin-bottom: 12px;
}
.storage-card-title {
  margin: 0 0 8px;
  font-size: 14px;
}
.storage-headline {
  margin: 0 0 8px;
  font-size: 18px;
  font-weight: 600;
}
.storage-facts {
  margin: 0;
  padding-left: 18px;
  color: var(--el-text-color-regular, #606266);
  line-height: 1.8;
}
.storage-note {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.storage-over {
  color: var(--el-color-warning, #e6a23c);
}
.storage-quota-form {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
}
.storage-quota-unit {
  color: var(--el-text-color-regular, #606266);
}
/* G7 备份引导入口 */
.storage-backup-link {
  margin-top: 8px;
}

/* M15 叶子模式（仅移动端渲染）：设备管理入口行 */
.storage-leaf-link {
  padding: 0 16px 12px;
}
</style>
