<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>身份备份</h2>
    </template>
    <p class="hint">同一身份的两种备份形式：加密二维码（便捷恢复，可放相册，需配合登录密码）与助记词（离线兜底，最高权限）。</p>
    <el-alert
      v-if="!backupMarked"
      title="尚未完成备份：建议立即保存备份二维码或抄写助记词，避免设备损坏后无法找回账号。"
      type="warning"
      :closable="false"
      show-icon
      class="block-gap"
    />
    <div class="row">
      <el-button :loading="qrLoading" @click="showBackupQr">显示备份二维码</el-button>
      <el-button @click="showRevealDialog = true">查看助记词</el-button>
    </div>

    <el-dialog v-model="showQrDialog" title="备份二维码" width="340px" append-to-body>
      <div class="qr-wrap">
        <img v-if="qrImageUrl" :src="qrImageUrl" alt="备份二维码" class="qr-image" />
      </div>
      <p class="hint">这是同一身份的加密备份，恢复时需输入登录密码。可保存到相册或发送给自己——内容已加密，但请勿与密码存放在一起。</p>
      <div class="row">
        <el-button type="primary" @click="saveQrImage">保存图片</el-button>
      </div>
    </el-dialog>

    <el-dialog v-model="showRevealDialog" title="查看助记词" width="440px" append-to-body @closed="resetReveal">
      <template v-if="!revealedMnemonic">
        <p class="hint">输入登录密码以查看助记词。助记词是账号最高权限，请确认周围无人窥屏。</p>
        <el-input
          v-model="revealPassword"
          type="password"
          show-password
          placeholder="登录密码"
          class="block-gap"
          @keyup.enter="revealMnemonic"
        />
        <div class="row">
          <el-button type="primary" :loading="revealBusy" @click="revealMnemonic">确认查看</el-button>
        </div>
      </template>
      <template v-else>
        <el-alert title="请离线抄写并妥善保存，不要截图、拍照或通过网络发送。" type="warning" :closable="false" show-icon />
        <div class="mnemonic-grid">
          <span v-for="(word, index) in revealedWords" :key="index" class="mnemonic-word">
            <em>{{ index + 1 }}</em>
            {{ word }}
          </span>
        </div>
        <div class="row">
          <el-button type="primary" @click="copyRevealedMnemonic">复制助记词</el-button>
        </div>
      </template>
    </el-dialog>
  </el-card>
</template>

<script lang="ts">
/**
 * 身份备份卡片（自 MinePage.vue 抽出，纯结构移动）。
 *
 * 职责：加密二维码备份（qrcode.toDataURL）+ 密码门控助记词查看；
 * 备份完成标记随 rootId（prop）变化自动刷新（等价于原 MinePage
 * onMounted / syncAuthState 中的 refreshBackupMarked 调用）。
 */
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import QRCode from 'qrcode';
import { isIdentityBackupMarked, markIdentityBackupDone } from '../utils/backup-state';
import { errorMessage } from '../utils/ipc';

export default defineComponent({
  name: 'BackupCard',
  props: {
    rootId: {
      type: String as PropType<string | null>,
      default: null
    }
  },
  setup(props) {
    // ---------------- 身份备份（加密二维码 + 密码门控助记词） ----------------
    const backupMarked = ref(true);
    const showQrDialog = ref(false);
    const qrImageUrl = ref('');
    const qrLoading = ref(false);
    const showRevealDialog = ref(false);
    const revealPassword = ref('');
    const revealBusy = ref(false);
    const revealedMnemonic = ref('');

    const refreshBackupMarked = () => {
      backupMarked.value = isIdentityBackupMarked(props.rootId);
    };

    const markBackupDone = () => {
      if (props.rootId) {
        markIdentityBackupDone(props.rootId);
        backupMarked.value = true;
      }
    };

    const showBackupQr = async () => {
      qrLoading.value = true;
      try {
        const { payload } = await window.electronAPI.rootIdentity.backupPayload();
        qrImageUrl.value = await QRCode.toDataURL(payload, { errorCorrectionLevel: 'M', margin: 1, width: 280 });
        showQrDialog.value = true;
        markBackupDone();
      } catch (error) {
        ElMessage.error(`生成备份二维码失败：${errorMessage(error)}`);
      } finally {
        qrLoading.value = false;
      }
    };

    const saveQrImage = () => {
      const link = document.createElement('a');
      link.href = qrImageUrl.value;
      link.download = `spark-backup-${(props.rootId ?? 'root').slice(0, 8)}.png`;
      link.click();
    };

    const revealMnemonic = async () => {
      if (!revealPassword.value) {
        ElMessage.warning('请输入登录密码');
        return;
      }
      revealBusy.value = true;
      try {
        const result = await window.electronAPI.rootIdentity.revealMnemonic(revealPassword.value);
        revealedMnemonic.value = result.mnemonic;
        markBackupDone();
      } catch (error) {
        ElMessage.error(`查看失败：${errorMessage(error)}`);
      } finally {
        revealBusy.value = false;
      }
    };

    const revealedWords = computed(() => (revealedMnemonic.value ? revealedMnemonic.value.split(' ') : []));

    const copyRevealedMnemonic = async () => {
      try {
        await navigator.clipboard.writeText(revealedMnemonic.value);
        ElMessage.success('已复制助记词');
      } catch {
        ElMessage.warning('复制失败，请手动抄写');
      }
    };

    const resetReveal = () => {
      revealPassword.value = '';
      revealedMnemonic.value = '';
    };

    // 等价于原 MinePage onMounted / syncAuthState 中的 refreshBackupMarked 调用
    watch(() => props.rootId, refreshBackupMarked, { immediate: true });

    return {
      backupMarked,
      showQrDialog,
      qrImageUrl,
      qrLoading,
      showRevealDialog,
      revealPassword,
      revealBusy,
      revealedMnemonic,
      revealedWords,
      showBackupQr,
      saveQrImage,
      revealMnemonic,
      copyRevealedMnemonic,
      resetReveal
    };
  }
});
</script>

<!-- 二维码/助记词样式原在 mine.css，MinePage 以 scoped 引入后对子组件不生效，挪入本组件；.row 与 mine.css 中的同名规则一致（MinePage 自身仍在用） -->
<style scoped>
.row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-top: 12px;
  align-items: center;
}

.mnemonic-grid {
  display: grid;
  grid-template-columns: repeat(6, 1fr);
  gap: 8px;
  margin: 14px 0;
}

.mnemonic-word {
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  padding: 6px 0;
  text-align: center;
  font-size: 16px;
  background: var(--spark-bg-hover);
}

.mnemonic-word em {
  display: block;
  font-style: normal;
  font-size: 10px;
  color: var(--spark-text-3);
}

.qr-wrap {
  display: flex;
  justify-content: center;
  margin-bottom: 12px;
}

.qr-image {
  width: 280px;
  height: 280px;
  image-rendering: pixelated;
}
</style>
