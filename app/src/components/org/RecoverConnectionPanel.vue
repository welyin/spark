<!-- 职责：组织详情抽屉内的「恢复连接」面板——节点名片二维码分享 + 粘贴/图片解码导入 -->
<template>
  <!-- 恢复连接（org.md §17 节点名片）：组织失联时的手动找回通道。
       TODO: 移动端仅支持扫码导入（无文本框），平台分支后续版本接入 -->
  <h3 class="section-title">恢复连接</h3>
  <p class="hint">
    组织失联时，可通过节点名片手动找回成员：把你的名片发给在线成员，或粘贴/扫描对方的名片。
    名片 10 分钟内有效；导入的节点按未验证处理，成员资格仍走原有校验。
  </p>
  <div class="recovery-actions">
    <el-button
      size="small"
      :type="recoveryPanel === 'share' ? 'primary' : 'default'"
      @click="toggleRecoveryPanel('share')"
    >
      分享我的节点
    </el-button>
    <el-button
      size="small"
      :type="recoveryPanel === 'import' ? 'primary' : 'default'"
      @click="toggleRecoveryPanel('import')"
    >
      手动添加节点
    </el-button>
  </div>

  <div v-if="recoveryPanel === 'share'" class="recovery-panel">
    <el-checkbox v-model="shareWithToken">
      附带本组织恢复 token（对方可用它帮助找回组织其他成员）
    </el-checkbox>
    <el-button
      type="primary"
      size="small"
      :loading="makingCard"
      style="margin-top: 8px"
      @click="makeNodeCard"
    >
      生成节点名片
    </el-button>
    <div v-if="nodeCard" class="node-card-result">
      <img v-if="nodeCardQr" :src="nodeCardQr" class="node-card-qr" alt="节点名片二维码" />
      <el-input v-model="nodeCard" type="textarea" :rows="3" readonly class="node-card-text" />
      <el-button text type="primary" size="small" @click="copyNodeCard">复制名片串</el-button>
    </div>
  </div>

  <div v-if="recoveryPanel === 'import'" class="recovery-panel">
    <el-input
      v-model="importCardText"
      type="textarea"
      :rows="3"
      placeholder="粘贴对方分享的节点名片串"
    />
    <div class="recovery-import-actions">
      <el-button size="small" @click="triggerCardFileSelect">从二维码图片读取</el-button>
      <el-button
        type="primary"
        size="small"
        :loading="importingCard"
        :disabled="!importCardText.trim()"
        @click="importNodeCard"
      >
        添加节点
      </el-button>
    </div>
    <input
      ref="cardFileInput"
      type="file"
      accept="image/*"
      style="display: none"
      @change="onCardFileChange"
    />
  </div>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import QRCode from 'qrcode';
import jsQR from 'jsqr';

export default defineComponent({
  name: 'RecoverConnectionPanel',
  props: {
    orgId: { type: String, required: true }
  },
  setup(props, { expose }) {
    // ------------------------------------------------------------------
    // 恢复连接（org.md §17 节点名片）
    // TODO: 移动端仅支持扫码导入（无文本框输入），平台分支后续版本接入
    // ------------------------------------------------------------------
    const recoveryPanel = ref<'' | 'share' | 'import'>('');
    const shareWithToken = ref(true);
    const makingCard = ref(false);
    const nodeCard = ref('');
    const nodeCardQr = ref('');
    const importCardText = ref('');
    const importingCard = ref(false);
    const cardFileInput = ref<HTMLInputElement | null>(null);

    const toggleRecoveryPanel = (panel: 'share' | 'import') => {
      recoveryPanel.value = recoveryPanel.value === panel ? '' : panel;
    };

    const makeNodeCard = async () => {
      if (!props.orgId) {
        return;
      }
      makingCard.value = true;
      try {
        const orgId = shareWithToken.value ? props.orgId : undefined;
        const result = await window.electronAPI.p2p.makeNodeCard(orgId);
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
        makingCard.value = false;
      }
    };

    const copyNodeCard = async () => {
      try {
        await navigator.clipboard.writeText(nodeCard.value);
        ElMessage.success('节点名片已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const triggerCardFileSelect = () => {
      cardFileInput.value?.click();
    };

    // 二维码图片 → jsqr 解码 → 填入名片串（RecoverPage.vue 同款模式）
    const onCardFileChange = async (event: Event) => {
      const input = event.target as HTMLInputElement;
      const file = input.files?.[0];
      input.value = '';
      if (!file) {
        return;
      }
      try {
        const bitmap = await createImageBitmap(file);
        const canvas = document.createElement('canvas');
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        const context = canvas.getContext('2d');
        if (!context) {
          throw new Error('no-canvas');
        }
        context.drawImage(bitmap, 0, 0);
        const imageData = context.getImageData(0, 0, canvas.width, canvas.height);
        const decoded = jsQR(imageData.data, imageData.width, imageData.height);
        if (!decoded?.data) {
          ElMessage.warning('无法识别图片中的二维码，请确认图片清晰完整');
          return;
        }
        importCardText.value = decoded.data;
      } catch {
        ElMessage.error('图片读取失败，请换一张图片重试');
      }
    };

    const importNodeCard = async () => {
      const card = importCardText.value.trim();
      if (!card) {
        return;
      }
      importingCard.value = true;
      try {
        const result = await window.electronAPI.p2p.importNodeCard(card);
        if (result.connectError) {
          ElMessage.warning(
            `节点 ${result.peerId.slice(0, 16)}... 已加入邻居池（未验证），但连接失败：${result.connectError}。节点在线后会自动重试。`
          );
        } else {
          ElMessage.success(`已添加节点 ${result.peerId.slice(0, 16)}... 并完成连接`);
        }
        importCardText.value = '';
      } catch (error) {
        ElMessage.error(`添加节点失败：${error}`);
      } finally {
        importingCard.value = false;
      }
    };

    /** 打开组织详情时由 OrgPage 调用：收起面板并清空名片内容 */
    const reset = () => {
      recoveryPanel.value = '';
      nodeCard.value = '';
      nodeCardQr.value = '';
      importCardText.value = '';
    };

    expose({ reset });

    return {
      recoveryPanel,
      shareWithToken,
      makingCard,
      nodeCard,
      nodeCardQr,
      importCardText,
      importingCard,
      cardFileInput,
      toggleRecoveryPanel,
      makeNodeCard,
      copyNodeCard,
      triggerCardFileSelect,
      onCardFileChange,
      importNodeCard
    };
  }
});
</script>
