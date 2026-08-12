<template>
  <div class="security-panel">
    <p class="panel-desc">使用指纹或人脸快速解锁当前身份。解锁凭据仅保存在本机安全芯片/Keystore 中，不会上传服务器。</p>

    <div v-if="statusLoading" class="status-row">正在检查生物识别状态…</div>
    <template v-else>
      <div class="status-row">
        <span class="status-label">设备支持</span>
        <span :class="['status-value', env.available ? 'ok' : 'bad']">{{ env.available ? '支持' : '不支持' }}</span>
      </div>
      <div class="status-row">
        <span class="status-label">已录入生物特征</span>
        <span :class="['status-value', env.enrolled ? 'ok' : 'bad']">{{ env.enrolled ? '已录入' : '未录入' }}</span>
      </div>
      <div class="status-row">
        <span class="status-label">已保存解锁凭据</span>
        <span :class="['status-value', env.hasSecret ? 'ok' : 'bad']">{{ env.hasSecret ? '已保存' : '未保存' }}</span>
      </div>

      <el-alert
        v-if="!canUseBiometric"
        type="info"
        :closable="false"
        :description="disabledReason"
        class="bio-tip"
      />

      <template v-if="settingEnabled">
        <div class="setting-row">
          <span>使用生物识别解锁</span>
          <el-switch v-model="localEnabled" :disabled="!canUseBiometric" @change="onToggle" />
        </div>
        <el-alert
          v-if="settingEnabled && !canUseBiometric"
          type="warning"
          :closable="false"
          title="生物识别凭据已失效"
          description="当前保存的凭据不可用，请关闭后使用密码登录，再重新开启。"
          class="bio-tip"
        />
      </template>

      <template v-else>
        <el-form label-position="top" class="enable-form">
          <el-form-item label="输入当前密码以开启">
            <el-input v-model="currentPassword" type="password" show-password placeholder="当前密码" :disabled="!canUseBiometric" />
          </el-form-item>
        </el-form>
        <div class="panel-actions">
          <el-button @click="emit('cancel')">取消</el-button>
          <el-button type="primary" :loading="loading" :disabled="!canUseBiometric || !currentPassword" @click="enable">
            开启生物识别解锁
          </el-button>
        </div>
      </template>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import {
  biometricCheck,
  biometricDelete,
  biometricErrorMessage,
  biometricInvalidate,
  biometricStorePassword,
} from '../../utils/biometric';
import { isBiometricUnlockEnabled, setBiometricUnlockEnabled } from '../../utils/biometric-setting';
import type { BiometricStatusDto } from '../../api/types';

const props = defineProps<{ rootId?: string }>();
const emit = defineEmits<{ (e: 'cancel'): void }>();

const statusLoading = ref(false);
const loading = ref(false);
const env = ref<BiometricStatusDto>({ available: false, enrolled: false, hasSecret: false });
const settingEnabled = ref(false);
const localEnabled = ref(false);
const currentPassword = ref('');

// 首次开启只需要设备支持 + 已录入生物特征；hasSecret 仅用于状态展示和已开启态的失效检测
const canUseBiometric = computed(() => env.value.available && env.value.enrolled);

const disabledReason = computed(() => {
  if (!env.value.available) return '当前设备或平台不支持生物识别';
  if (!env.value.enrolled) return '尚未录入指纹/人脸，请在系统设置中录入后再开启';
  return '';
});

async function refreshEnv() {
  statusLoading.value = true;
  try {
    env.value = await biometricCheck();
  } catch (err) {
    ElMessage.error(biometricErrorMessage(err));
  } finally {
    statusLoading.value = false;
  }
}

onMounted(async () => {
  settingEnabled.value = isBiometricUnlockEnabled();
  localEnabled.value = settingEnabled.value;
  await refreshEnv();
});

watch(
  () => env.value.hasSecret,
  (hasSecret) => {
    if (settingEnabled.value && !hasSecret) {
      // 凭据失效：自动关闭开关并提示
      biometricInvalidate();
      settingEnabled.value = false;
      localEnabled.value = false;
    }
  }
);

async function enable() {
  if (!props.rootId || !currentPassword.value) return;
  loading.value = true;
  try {
    await biometricStorePassword(currentPassword.value);
    setBiometricUnlockEnabled(true);
    settingEnabled.value = true;
    localEnabled.value = true;
    currentPassword.value = '';
    ElMessage.success('生物识别解锁已开启');
    await refreshEnv();
  } catch (err) {
    ElMessage.error(biometricErrorMessage(err));
  } finally {
    loading.value = false;
  }
}

async function onToggle(val: string | number | boolean) {
  const enabled = !!val;
  if (enabled) {
    // 这里只在失效后重新开启；正常开启走密码表单
    localEnabled.value = false;
    return;
  }
  // 关闭
  loading.value = true;
  try {
    await biometricDelete();
    setBiometricUnlockEnabled(false);
    settingEnabled.value = false;
    localEnabled.value = false;
    ElMessage.success('已关闭生物识别解锁');
  } catch (err) {
    ElMessage.error(biometricErrorMessage(err));
    localEnabled.value = true;
  } finally {
    loading.value = false;
  }
}
</script>

<style scoped>
.security-panel {
  padding: 8px 0;
}
.panel-desc {
  margin: 0 0 16px;
  color: var(--spark-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}
.status-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 0;
  border-bottom: 1px solid var(--spark-border-light);
  font-size: 14px;
}
.status-label {
  color: var(--spark-text-secondary);
}
.status-value {
  font-weight: 500;
}
.status-value.ok {
  color: var(--spark-success);
}
.status-value.bad {
  color: var(--spark-danger);
}
.bio-tip {
  margin-top: 12px;
}
.setting-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 16px;
  font-size: 14px;
}
.enable-form {
  margin-top: 16px;
}
.panel-actions {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 16px;
}
</style>
