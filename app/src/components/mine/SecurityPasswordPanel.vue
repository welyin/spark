<template>
  <div class="security-panel">
    <p class="panel-desc">修改当前身份口令，修改后其他已登录设备需要重新验证。</p>
    <div class="security-form">
      <el-input v-model="oldPassword" type="password" show-password placeholder="当前密码" @keyup.enter="submit" />
      <el-input v-model="newPassword" type="password" show-password placeholder="新密码（至少 8 位）" @keyup.enter="submit" />
      <PasswordStrengthMeter :password="newPassword" />
      <el-input v-model="confirmPassword" type="password" show-password placeholder="确认新密码" @keyup.enter="submit" />
      <div class="security-form-actions">
        <el-button type="primary" :loading="loading" :disabled="!canSubmit" @click="submit">修改密码</el-button>
      </div>
      <el-alert v-if="formMessage" :title="formMessage" type="error" :closable="false" show-icon />
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { errorMessage } from '../../utils/ipc';
import PasswordStrengthMeter from '../common/PasswordStrengthMeter.vue';

const emit = defineEmits<{
  (e: 'cancel'): void;
  (e: 'success', newPassword: string): void;
}>();

const oldPassword = ref('');
const newPassword = ref('');
const confirmPassword = ref('');
const loading = ref(false);
const formMessage = ref('');

const canSubmit = computed(
  () => oldPassword.value.length > 0 && newPassword.value.length >= 8 && newPassword.value === confirmPassword.value
);

async function submit() {
  formMessage.value = '';
  if (!oldPassword.value) {
    formMessage.value = '请填写当前密码';
    return;
  }
  if (newPassword.value.length < 8) {
    formMessage.value = '新密码至少 8 位';
    return;
  }
  if (newPassword.value !== confirmPassword.value) {
    formMessage.value = '两次输入的新密码不一致';
    return;
  }

  loading.value = true;
  try {
    await window.electronAPI.rootIdentity.changePassword(oldPassword.value, newPassword.value);
    const emittedPassword = newPassword.value;
    oldPassword.value = '';
    newPassword.value = '';
    confirmPassword.value = '';
    emit('success', emittedPassword);
  } catch (err) {
    formMessage.value = `修改失败：${errorMessage(err)}`;
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
.security-form {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 14px;
  margin-top: 12px;
  max-width: 360px;
}
.security-form .el-input {
  width: 100%;
}
.security-form-actions {
  display: flex;
  gap: 12px;
}
</style>
