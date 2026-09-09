<!-- 个人过滤规则设置（communication §4.3，product/todo #22①）：
     关键词 / 来源屏蔽规则的 CRUD + JSON 导入导出。
     自我审查定位如实告知：本地过滤只影响自己的视图，不影响他人与网络。 -->
<template>
  <el-dialog
    :model-value="modelValue"
    title="个人过滤规则"
    width="540px"
    @update:model-value="$emit('update:modelValue', $event)"
  >
    <!-- 如实口径（communication §4.3）：本地过滤的边界必须写清，不暗示能影响他人或网络 -->
    <el-alert
      type="info"
      :closable="false"
      class="filter-honest"
      title="本地过滤：规则只影响你自己在这台设备上的聊天视图，不影响对方与网络中的任何人。规则保存在本机插件数据域，可导出为 JSON 随身携带。"
    />

    <div class="filter-add">
      <el-input
        v-model="keywordInput"
        placeholder="关键词：消息包含该词即隐藏（大小写不敏感）"
        clearable
        @keyup.enter="onAddKeyword"
      >
        <template #append>
          <el-button @click="onAddKeyword">添加</el-button>
        </template>
      </el-input>
      <el-input
        v-model="sourceInput"
        placeholder="来源屏蔽：对方身份 ID，其发来的消息一律隐藏"
        clearable
        @keyup.enter="onAddSource"
      >
        <template #append>
          <el-button @click="onAddSource">添加</el-button>
        </template>
      </el-input>
    </div>

    <el-empty v-if="!rules.length" :image-size="60" description="暂无过滤规则" />
    <ul v-else class="filter-list">
      <li v-for="rule in rules" :key="rule.id" class="filter-item">
        <span class="filter-kind" :class="`is-${rule.kind}`">{{ rule.kind === 'keyword' ? '关键词' : '来源' }}</span>
        <span class="filter-target" :title="rule.target">{{ rule.target }}</span>
        <el-switch :model-value="rule.enabled" @change="(value: boolean) => setRuleEnabled(rule.id, value)" />
        <el-button text type="danger" :icon="Delete" title="删除规则" @click="removeFilterRule(rule.id)" />
      </li>
    </ul>

    <template #footer>
      <div class="filter-footer">
        <el-button :disabled="!rules.length" @click="onExportFile">导出 JSON 文件</el-button>
        <el-button :disabled="!rules.length" @click="onExportClipboard">复制到剪贴板</el-button>
        <el-button @click="onImportClick">导入…</el-button>
        <el-button type="danger" plain :disabled="!rules.length" @click="onClear">清空</el-button>
        <!-- 隐藏文件选择器：导入只走「导入…」按钮触发 -->
        <input
          ref="fileInputRef"
          type="file"
          accept=".json,application/json"
          class="filter-file-input"
          @change="onImportFile"
        />
      </div>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Delete } from '@element-plus/icons-vue';
import {
  addKeywordRule,
  addSourceRule,
  clearFilterRules,
  exportFilterRules,
  importFilterRules,
  listFilterRules,
  removeFilterRule,
  setRuleEnabled
} from '../filter-rules';

export default defineComponent({
  name: 'FilterSettings',
  props: {
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue'],
  setup() {
    const keywordInput = ref('');
    const sourceInput = ref('');
    const fileInputRef = ref<HTMLInputElement>();

    // state.rules 是模块级响应式缓存（filter-rules.ts），水合/增删后自动刷新
    const rules = computed(() => listFilterRules());

    function onAddKeyword() {
      if (addKeywordRule(keywordInput.value)) {
        keywordInput.value = '';
      } else if (keywordInput.value.trim()) {
        ElMessage.info('该关键词规则已存在');
      }
    }

    function onAddSource() {
      if (addSourceRule(sourceInput.value)) {
        sourceInput.value = '';
      } else if (sourceInput.value.trim()) {
        ElMessage.info('该来源屏蔽规则已存在');
      }
    }

    function onExportFile() {
      // Blob 下载在插件 iframe 沙箱内可用（不依赖剪贴板权限），文件名带日期便于归档
      const blob = new Blob([exportFilterRules()], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `spark-chat-filter-rules-${new Date().toISOString().slice(0, 10)}.json`;
      anchor.click();
      URL.revokeObjectURL(url);
    }

    async function onExportClipboard() {
      try {
        await navigator.clipboard.writeText(exportFilterRules());
        ElMessage.success('已复制到剪贴板');
      } catch {
        ElMessage.error('复制失败（可改用「导出 JSON 文件」）');
      }
    }

    function onImportClick() {
      fileInputRef.value?.click();
    }

    function onImportFile(event: Event) {
      const input = event.target as HTMLInputElement;
      const file = input.files?.[0];
      // 重置选择器：同一文件可再次导入（否则 change 不触发）
      input.value = '';
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        const result = importFilterRules(String(reader.result ?? ''));
        if (!result.ok) {
          // 畸形导入：拒绝且不改动现有规则（结构校验在 filter-rules 模型层）
          ElMessage.error(`导入失败：${result.reason}`);
          return;
        }
        ElMessage.success(
          result.skipped > 0 ? `已导入 ${result.imported} 条规则（${result.skipped} 条与现有重复已跳过）` : `已导入 ${result.imported} 条规则`
        );
      };
      reader.onerror = () => ElMessage.error('读取文件失败');
      reader.readAsText(file);
    }

    async function onClear() {
      try {
        await ElMessageBox.confirm('清空后可通过导入之前的导出文件还原。', '清空全部过滤规则？', {
          confirmButtonText: '清空',
          cancelButtonText: '取消',
          type: 'warning'
        });
        clearFilterRules();
      } catch {
        // 用户取消
      }
    }

    return {
      keywordInput,
      sourceInput,
      fileInputRef,
      rules,
      onAddKeyword,
      onAddSource,
      setRuleEnabled,
      removeFilterRule,
      onExportFile,
      onExportClipboard,
      onImportClick,
      onImportFile,
      onClear,
      Delete
    };
  }
});
</script>

<style scoped>
.filter-honest {
  margin-bottom: 12px;
}

.filter-add {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
}

.filter-list {
  list-style: none;
  margin: 0;
  padding: 0;
  max-height: 300px;
  overflow-y: auto;
}

.filter-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 0;
  border-bottom: 1px solid var(--spark-border-light, #ebeef5);
}

.filter-kind {
  flex-shrink: 0;
  font-size: 12px;
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--spark-bg-hover, #f5f7fa);
  color: var(--spark-text-2, #606266);
}

.filter-target {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.filter-footer {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.filter-file-input {
  display: none;
}
</style>
