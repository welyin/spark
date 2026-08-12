<template>
  <div class="security-panel">
    <template v-if="inboundRecovery">
      <el-alert type="warning" :closable="false" class="recovery-card">
        <template #title>
          <div class="card-title">来自「{{ inboundRecovery.fromDevice }}」的恢复请求</div>
        </template>
        <div class="card-body">
          <p>该设备正在发起密码重置。若不是你本人操作，请在倒计时结束前否决。</p>
          <p class="countdown">否决窗口剩余：{{ formatRemaining(remainingMs(inboundRecovery.deadline)) }}</p>
        </div>
        <div class="card-actions">
          <el-button type="danger" :loading="vetoLoading" @click="veto">否决此操作</el-button>
        </div>
      </el-alert>
    </template>

    <template v-else-if="pendingRecovery">
      <el-alert :type="recoveryReadyToConfirm ? 'success' : 'info'" :closable="false" class="recovery-card">
        <template #title>
          <div class="card-title">{{ recoveryReadyToConfirm ? '可以进行确认' : '等待公示期结束' }}</div>
        </template>
        <div class="card-body">
          <p>请求 ID：{{ pendingRecovery.requestId }}</p>
          <p v-if="!recoveryReadyToConfirm" class="countdown">
            确认窗口剩余：{{ formatRemaining(remainingMs(pendingRecovery.deadline)) }}
          </p>
          <p v-else>公示期已结束，验证生物识别后设置新密码。</p>
        </div>
        <div class="card-actions">
          <el-button v-if="recoveryReadyToConfirm" type="primary" :loading="ritualLoading" @click="startConfirm">
            确认重置
          </el-button>
          <el-button v-else text disabled>尚未到确认时间</el-button>
        </div>
      </el-alert>

      <el-form v-if="showConfirmForm" label-position="top" class="confirm-form">
        <el-form-item label="新密码">
          <el-input v-model="newPassword" type="password" show-password placeholder="至少 8 位" />
        </el-form-item>
        <el-form-item label="确认新密码">
          <el-input v-model="confirmPassword" type="password" show-password placeholder="再次输入新密码" />
        </el-form-item>
        <div class="panel-actions">
          <el-button @click="showConfirmForm = false">取消</el-button>
          <el-button type="primary" :loading="confirmLoading" :disabled="!passwordMatch" @click="submitConfirm">
            确认重置
          </el-button>
        </div>
      </el-form>
    </template>

    <template v-else>
      <p class="panel-desc">
        如果忘记密码，可在本设备发起延迟恢复。请求将进入 {{ delayHours }} 小时公示期，期间任一已登录设备都可否决；公示期结束后需再次验证生物识别并设置新密码。
      </p>
      <p class="panel-desc warn">请确保当前身份已开启生物识别解锁，否则无法完成确认。</p>
      <div class="panel-actions">
        <el-button @click="emit('cancel')">取消</el-button>
        <el-button type="warning" :loading="ritualLoading" @click="startInitiate">发起忘记密码重置</el-button>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { isBiometricUnlockEnabled } from '../../utils/biometric-setting';
import { runBiometricRitual } from '../../utils/biometric-ritual';
import {
  confirmRecovery,
  formatRemaining,
  hydrateRecovery,
  inboundRecovery,
  initiateRecovery,
  pendingRecovery,
  remainingMs,
  recoveryReadyToConfirm,
  vetoRecovery,
} from '../../stores/recovery';

const props = defineProps<{ rootId?: string }>();
const emit = defineEmits<{ (e: 'cancel'): void }>();

const DELAY_HOURS = 24;
const delayHours = DELAY_HOURS;

const ritualLoading = ref(false);
const vetoLoading = ref(false);
const confirmLoading = ref(false);
const showConfirmForm = ref(false);
const newPassword = ref('');
const confirmPassword = ref('');
let refreshTimer: ReturnType<typeof setInterval> | null = null;

const passwordMatch = computed(() => newPassword.value && newPassword.value === confirmPassword.value && newPassword.value.length >= 8);

async function refresh() {
  if (!props.rootId) return;
  await hydrateRecovery(props.rootId);
}

onMounted(() => {
  refresh();
  refreshTimer = setInterval(refresh, 30000);
});

onBeforeUnmount(() => {
  if (refreshTimer) {
    clearInterval(refreshTimer);
    refreshTimer = null;
  }
});

watch(pendingRecovery, (next) => {
  if (!next) {
    showConfirmForm.value = false;
    newPassword.value = '';
    confirmPassword.value = '';
  }
});

async function performRitual(): Promise<string | null> {
  if (!props.rootId) return null;
  const result = await runBiometricRitual(props.rootId);
  if (result.message) {
    ElMessage.warning(result.message);
  }
  return result.password;
}

async function startInitiate() {
  if (!props.rootId) return;
  try {
    await ElMessageBox.confirm(
      `发起后将进入 ${DELAY_HOURS} 小时公示期，期间任一设备都可否决。公示期结束后需在本机再次验证生物识别并设置新密码。`,
      '忘记密码重置',
      { confirmButtonText: '确认发起', cancelButtonText: '取消', type: 'warning' }
    );
  } catch {
    return;
  }

  ritualLoading.value = true;
  try {
    const password = await performRitual();
    if (password === null) return;
    await initiateRecovery('reset_password', DELAY_HOURS);
    await refresh();
  } finally {
    ritualLoading.value = false;
  }
}

async function startConfirm() {
  ritualLoading.value = true;
  try {
    const password = await performRitual();
    if (password === null) return;
    newPassword.value = '';
    confirmPassword.value = '';
    showConfirmForm.value = true;
  } finally {
    ritualLoading.value = false;
  }
}

async function submitConfirm() {
  if (!pendingRecovery.value) return;
  if (!passwordMatch.value) return;
  confirmLoading.value = true;
  try {
    await confirmRecovery(pendingRecovery.value.requestId, newPassword.value);
    showConfirmForm.value = false;
    await refresh();
  } finally {
    confirmLoading.value = false;
  }
}

async function veto() {
  if (!props.rootId || !inboundRecovery.value) return;
  vetoLoading.value = true;
  try {
    await vetoRecovery(props.rootId, inboundRecovery.value.requestId);
  } finally {
    vetoLoading.value = false;
  }
}
</script>

<style scoped>
.security-panel {
  padding: 8px 0;
}
.panel-desc {
  margin: 0 0 12px;
  color: var(--spark-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}
.panel-desc.warn {
  color: var(--spark-warning);
}
.recovery-card {
  margin-bottom: 16px;
}
.card-title {
  font-weight: 500;
  font-size: 14px;
}
.card-body p {
  margin: 6px 0;
  font-size: 13px;
  color: var(--spark-text-secondary);
}
.countdown {
  color: var(--spark-text-primary);
  font-weight: 500;
}
.card-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}
.confirm-form {
  margin-top: 16px;
}
.panel-actions {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 16px;
}
</style>
