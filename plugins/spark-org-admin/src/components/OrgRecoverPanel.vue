<!-- 找回组织面板（移植壳层 RecoverConnectionPanel）：节点名片分享（QR + 文本）
     与粘贴导入。二维码图片解码（jsQR 管线）未迁移——插件内仅支持文本粘贴导入，
     见任务报告缺口 -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>找回组织</h2>
    </template>
    <p class="hint">
      组织失联时，可通过节点名片手动找回成员：把你的名片发给在线成员，或粘贴对方的名片。
      名片 10 分钟内有效；导入的节点按未验证处理，成员资格仍走原有校验。
    </p>
    <div class="recovery-actions">
      <el-button size="small" :type="panel === 'share' ? 'primary' : 'default'" @click="toggle('share')">
        分享我的节点
      </el-button>
      <el-button size="small" :type="panel === 'import' ? 'primary' : 'default'" @click="toggle('import')">
        手动添加节点
      </el-button>
    </div>

    <div v-if="panel === 'share'" class="recovery-panel">
      <el-checkbox v-model="shareWithToken">
        附带本组织恢复 token（对方可用它帮助找回组织其他成员）
      </el-checkbox>
      <el-button type="primary" size="small" :loading="making" class="recovery-make" @click="makeCard">
        生成节点名片
      </el-button>
      <div v-if="nodeCard" class="node-card-result">
        <img v-if="nodeCardQr" :src="nodeCardQr" class="node-card-qr" alt="节点名片二维码" />
        <el-input v-model="nodeCard" type="textarea" :rows="3" readonly class="node-card-text" />
        <el-button text type="primary" size="small" @click="copyCard">复制名片串</el-button>
      </div>
    </div>

    <div v-if="panel === 'import'" class="recovery-panel">
      <el-input v-model="importText" type="textarea" :rows="3" placeholder="粘贴对方分享的节点名片串" />
      <div class="recovery-import-actions">
        <el-button type="primary" size="small" :loading="importing" :disabled="!importText.trim()" @click="importCard">
          添加节点
        </el-button>
      </div>
    </div>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import QRCode from 'qrcode';
import { orgApi } from '../sdk-host';

export default defineComponent({
  name: 'OrgRecoverPanel',
  props: {
    orgId: { type: String, required: true }
  },
  setup(props) {
    const panel = ref<'' | 'share' | 'import'>('');
    const shareWithToken = ref(true);
    const making = ref(false);
    const nodeCard = ref('');
    const nodeCardQr = ref('');
    const importText = ref('');
    const importing = ref(false);

    const toggle = (next: 'share' | 'import') => {
      panel.value = panel.value === next ? '' : next;
    };

    const makeCard = async () => {
      const api = orgApi();
      if (!api || !props.orgId) {
        return;
      }
      making.value = true;
      try {
        const result = await api.makeNodeCard(shareWithToken.value ? props.orgId : undefined);
        nodeCard.value = result.card;
        nodeCardQr.value = await QRCode.toDataURL(result.card, {
          errorCorrectionLevel: 'M',
          margin: 1,
          width: 220
        });
      } catch (error) {
        nodeCard.value = '';
        nodeCardQr.value = '';
        ElMessage.error(`生成节点名片失败：${error}`);
      } finally {
        making.value = false;
      }
    };

    const copyCard = async () => {
      try {
        await navigator.clipboard.writeText(nodeCard.value);
        ElMessage.success('节点名片已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const importCard = async () => {
      const api = orgApi();
      const card = importText.value.trim();
      if (!api || !card) {
        return;
      }
      importing.value = true;
      try {
        const result = await api.importNodeCard(card);
        if (result.connectError) {
          ElMessage.warning(
            `节点 ${result.peerId.slice(0, 16)}... 已加入邻居池（未验证），但连接失败：${result.connectError}。节点在线后会自动重试。`
          );
        } else {
          ElMessage.success(`已添加节点 ${result.peerId.slice(0, 16)}... 并完成连接`);
        }
        importText.value = '';
      } catch (error) {
        ElMessage.error(`添加节点失败：${error}`);
      } finally {
        importing.value = false;
      }
    };

    return {
      panel,
      shareWithToken,
      making,
      nodeCard,
      nodeCardQr,
      importText,
      importing,
      toggle,
      makeCard,
      copyCard,
      importCard
    };
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

.recovery-actions {
  display: flex;
  gap: 8px;
}

.recovery-panel {
  margin-top: 12px;
}

.recovery-make {
  margin-top: 8px;
  display: block;
}

.node-card-result {
  margin-top: 12px;
}

.node-card-qr {
  display: block;
  width: 220px;
  height: 220px;
  margin-bottom: 8px;
}

.node-card-text {
  margin-bottom: 4px;
}

.recovery-import-actions {
  margin-top: 8px;
}
</style>
