<!-- 策略配置面板（A42）：复用既有 sdk.policy 桥面——本地草稿读取/提交
     （结构/引擎校验 + 静态分析 findings）/ 发布（附组织签名包落同步键域，
     仅名册 admin；m-of-n 多签收集流不在本面，内核如实报错） -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>策略配置</h2>
    </template>
    <p class="hint">
      策略文档（B1 求值器在内核）控制组织内数据面的读写门禁。编辑 JSON 草稿后先「校验并保存草稿」，
      确认无 error 级发现项再「发布」同步给全体成员。发布须本机为名册管理员。
    </p>

    <el-input
      v-model="draftText"
      type="textarea"
      :rows="14"
      class="policy-editor"
      placeholder='粘贴或编辑策略文档 JSON，例如 { "v": 1, ... }'
      spellcheck="false"
    />
    <p v-if="draftSavedAt" class="hint">本地草稿保存于 {{ formatDate(draftSavedAt) }}（hash {{ draftHash.slice(0, 12) }}…）</p>

    <div class="policy-actions">
      <el-button :loading="loading" @click="loadDraft">读取草稿</el-button>
      <el-button type="primary" plain :loading="submitting" @click="submitDraft">校验并保存草稿</el-button>
      <el-button v-if="isAdmin" type="primary" :loading="publishing" @click="publish">发布到组织</el-button>
      <span v-else class="hint">仅管理员可发布</span>
    </div>

    <template v-if="findings.length">
      <h3 class="section-title">静态分析</h3>
      <div v-for="(finding, index) in findings" :key="index" class="policy-finding">
        <el-tag size="small" :type="finding.severity === 'error' ? 'danger' : 'warning'">
          {{ finding.severity === 'error' ? '错误' : '警告' }}
        </el-tag>
        <code>{{ finding.code }}</code>
        <span>{{ finding.detail }}</span>
      </div>
    </template>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PolicyFinding } from '../../../../packages/plugin-sdk/src';
import { policyApi } from '../sdk-host';
import { formatDate, parsePolicyDoc } from '../org-store';

export default defineComponent({
  name: 'OrgPolicyPanel',
  props: {
    orgId: { type: String, required: true },
    isAdmin: { type: Boolean, required: true }
  },
  setup(props) {
    const draftText = ref('');
    const draftHash = ref('');
    const draftSavedAt = ref(0);
    const findings = ref<PolicyFinding[]>([]);
    const loading = ref(false);
    const submitting = ref(false);
    const publishing = ref(false);

    const loadDraft = async () => {
      const api = policyApi();
      if (!api) {
        return;
      }
      loading.value = true;
      try {
        const draft = await api.read(props.orgId);
        if (draft) {
          draftText.value = JSON.stringify(draft.doc, null, 2);
          draftHash.value = draft.policyDocHash;
          draftSavedAt.value = draft.savedAt;
        } else {
          draftText.value = '';
          draftHash.value = '';
          draftSavedAt.value = 0;
        }
      } catch (error) {
        ElMessage.error(`读取策略草稿失败：${error}`);
      } finally {
        loading.value = false;
      }
    };

    const submitDraft = async () => {
      const api = policyApi();
      if (!api) {
        return;
      }
      const parsed = parsePolicyDoc(draftText.value);
      if ('error' in parsed) {
        ElMessage.warning(parsed.error);
        return;
      }
      submitting.value = true;
      try {
        const result = await api.submitDraft(parsed.doc);
        findings.value = result.findings;
        draftHash.value = result.policyDocHash;
        draftSavedAt.value = Date.now();
        const errors = result.findings.filter((finding) => finding.severity === 'error').length;
        ElMessage[errors ? 'warning' : 'success'](
          errors ? `草稿已保存，但有 ${errors} 个错误级发现项，发布前请修正` : '草稿已保存，静态分析通过'
        );
      } catch (error) {
        ElMessage.error(`提交草稿失败：${error}`);
      } finally {
        submitting.value = false;
      }
    };

    const publish = async () => {
      const api = policyApi();
      if (!api) {
        return;
      }
      try {
        await ElMessageBox.confirm(
          '发布将把本地草稿附组织签名包写入组织同步键域，全体成员随后同步到同一策略文档。确认发布？',
          '发布策略',
          { type: 'warning', confirmButtonText: '发布', cancelButtonText: '取消' }
        );
      } catch {
        return;
      }
      publishing.value = true;
      try {
        const result = await api.publish(props.orgId);
        ElMessage.success(
          result.degraded
            ? `已发布（legacy 组织降级证明，hash ${result.policyDocHash.slice(0, 12)}…）`
            : `已发布（hash ${result.policyDocHash.slice(0, 12)}…）`
        );
      } catch (error) {
        ElMessage.error(`发布失败：${error}`);
      } finally {
        publishing.value = false;
      }
    };

    onMounted(loadDraft);

    return {
      draftText,
      draftHash,
      draftSavedAt,
      findings,
      loading,
      submitting,
      publishing,
      loadDraft,
      submitDraft,
      publish,
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
  margin: 16px 0 8px;
  font-size: 14px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.policy-editor :deep(textarea) {
  font-family: var(--el-font-family-mono, monospace);
  font-size: 13px;
}

.policy-actions {
  margin-top: 12px;
  display: flex;
  align-items: center;
  gap: 8px;
}

.policy-finding {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
}
</style>
