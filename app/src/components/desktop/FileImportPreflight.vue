<!-- 导入预检（problem X5）：OS 文件拖入松手后的确认环节。
     已具备：数量清单 / 目标域明示 / 组织域「将同步给成员」口径 / 可取消。
     能力缺口（诚实标注，不伪造）：Tauri drag-drop 只交付本机路径，文件大小/类型/递归展开、
     流式进度、哈希去重、「先落本机再 P2P」均需内核文件应用/文件导入命令支持
     （src/api 当前无此类命令），确认后仅派发 spark:file-import-request 事件。 -->
<template>
  <el-dialog
    :model-value="!!preflight"
    title="导入文件"
    width="min(480px, 92vw)"
    :close-on-click-modal="false"
    @update:model-value="onVisibleChange"
  >
    <template v-if="preflight">
      <p class="preflight-target">
        目标域：<strong>{{ targetLabel }}</strong>
        <span class="preflight-scope">{{ scopeLabel }}</span>
      </p>
      <p v-if="preflight.target.type === 'org'" class="preflight-org-notice">
        <el-icon :size="14"><WarningFilled /></el-icon>
        组织域文件将同步给该组织成员。
      </p>
      <p class="preflight-count">共 {{ preflight.paths.length }} 个文件：</p>
      <ul class="preflight-list">
        <li v-for="path in shownPaths" :key="path" :title="path">{{ fileName(path) }}</li>
        <li v-if="preflight.paths.length > MAX_LISTED" class="preflight-more">
          … 等 {{ preflight.paths.length }} 项
        </li>
      </ul>
      <el-alert
        v-if="preflight.stage === 'precheck'"
        type="info"
        :closable="false"
        show-icon
        title="文件大小 / 类型 / 目录递归展开将在落盘时核对；当前文件导入收件箱能力待文件应用与内核支持，确认后暂不会实际写入。"
      />
      <el-alert
        v-else
        type="warning"
        :closable="false"
        show-icon
        title="已记录导入请求；因文件应用导入收件箱能力尚未就绪，本次未实际写入文件。"
      />
    </template>
    <template #footer>
      <template v-if="preflight && preflight.stage === 'precheck'">
        <el-button @click="cancel">取消</el-button>
        <el-button type="primary" @click="confirm">确认导入</el-button>
      </template>
      <el-button v-else type="primary" @click="cancel">知道了</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { WarningFilled } from '@element-plus/icons-vue';
import {
  closeImportPreflight,
  confirmImportPreflight,
  importPreflight,
  scopeLabelOf,
  spaceLabelOf
} from '../../stores/os-file-drop';

export default defineComponent({
  name: 'FileImportPreflight',
  components: { WarningFilled },
  setup() {
    const MAX_LISTED = 8;
    const preflight = importPreflight;
    const targetLabel = computed(() => (preflight.value ? spaceLabelOf(preflight.value.target) : ''));
    const scopeLabel = computed(() => (preflight.value ? scopeLabelOf(preflight.value.target) : ''));
    const shownPaths = computed(() => preflight.value?.paths.slice(0, MAX_LISTED) ?? []);
    /** 路径尾段展示（不解析目录结构——递归展开属内核能力） */
    const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

    const confirm = () => confirmImportPreflight();
    const cancel = () => closeImportPreflight();
    const onVisibleChange = (visible: boolean) => {
      if (!visible) cancel();
    };

    return { preflight, targetLabel, scopeLabel, shownPaths, MAX_LISTED, fileName, confirm, cancel, onVisibleChange };
  }
});
</script>

<style scoped>
.preflight-target {
  margin: 0 0 8px;
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
}

.preflight-scope {
  margin-left: 8px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.preflight-org-notice {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0 0 8px;
  font-size: var(--spark-font-size-secondary);
  color: var(--el-color-warning);
}

.preflight-count {
  margin: 0 0 4px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-2);
}

.preflight-list {
  margin: 0 0 12px;
  padding: 8px 12px;
  max-height: 160px;
  overflow-y: auto;
  list-style: none;
  background: var(--spark-bg-hover);
  border-radius: var(--spark-radius-m);
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-1);
  line-height: 1.8;
}

.preflight-more {
  color: var(--spark-text-3);
}
</style>
