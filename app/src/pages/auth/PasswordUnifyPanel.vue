<template>
  <section class="auth-panel">
    <h2 class="auth-title">统一新密码</h2>
    <p class="hint">{{ bannerText }}</p>

    <el-alert
      v-if="state.reason === 'password_reset'"
      type="warning"
      title="密码已被重置"
      description="该账号的密码通过延迟恢复通道被重置。若非本人操作，请立即检查设备管理与安全日志。"
      :closable="false"
      show-icon
      class="block-gap"
    />

    <el-form label-position="top" class="auth-form" @submit.prevent>
      <el-form-item label="当前设备上的旧密码">
        <el-input ref="oldInput" v-model="oldPassword" type="password" show-password placeholder="在本设备还能登录的那个密码" :disabled="busy" @keydown.enter.prevent="submit" />
      </el-form-item>
      <el-form-item label="设密设备上的新密码">
        <el-input v-model="newPassword" type="password" show-password placeholder="其他设备上设的新密码" :disabled="busy" @keydown.enter.prevent="submit" />
      </el-form-item>
      <el-form-item label="确认新密码">
        <el-input v-model="confirmPassword" type="password" show-password placeholder="再次输入新密码" :disabled="busy" @keydown.enter.prevent="submit" />
      </el-form-item>
      <el-button class="submit-btn" type="primary" native-type="button" :loading="busy" :disabled="!canSubmit" @click="submit">确认统一</el-button>
      <el-button class="submit-btn" link native-type="button" :disabled="busy" @click="emit('back')">返回登录</el-button>
    </el-form>

    <el-alert v-if="message" :title="message" type="error" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { verifyPasswordTicket, unifyPassword, formatTs, type PendingUnify } from '../../stores/password-unify';
import { biometricRebindAfterUnify } from '../../utils/biometric-rebind';

export default defineComponent({
  name: 'PasswordUnifyPanel',
  props: {
    rootId: { type: String, default: '' }
  },
  emits: ['back', 'done'],
  setup(props, { emit }) {
    const state = ref<PendingUnify>({
      rotatedAt: Date.now(),
      rotatedBy: '其他设备',
      rotatedByDevice: '其他设备',
      reason: 'password_change'
    });

    const oldPassword = ref('');
    const newPassword = ref('');
    const confirmPassword = ref('');
    const busy = ref(false);
    const message = ref('');
    const oldInput = ref<{ focus: () => void } | null>(null);

    onMounted(() => {
      oldInput.value?.focus();
    });

    const bannerText = computed(() => {
      const when = formatTs(state.value.rotatedAt);
      return `『${state.value.rotatedByDevice}』于 ${when} 修改了密码。请用本设备还能登录的旧密码和设密设备上的新密码完成统一。`;
    });

    const canSubmit = computed(
      () => oldPassword.value.length > 0 && newPassword.value.length >= 8 && newPassword.value === confirmPassword.value
    );

    async function verifyTicketStep(): Promise<string | null> {
      message.value = '';
      const result = await verifyPasswordTicket(oldPassword.value);
      if (result.ok) {
        return oldPassword.value;
      }
      const pending = state.value;
      message.value = [
        `与『${pending.rotatedByDevice}』于 ${formatTs(pending.rotatedAt)} 设的新密码不一致。`,
        '请输入那台设备上设的新密码。',
        '想不起来？可先用旧密码登录，或在设密设备上重新修改密码。'
      ].join('\n');
      return null;
    }

    async function submit() {
      if (!canSubmit.value || busy.value) return;
      message.value = '';
      busy.value = true;
      try {
        // F4 纪律：验票不过不许重封
        const verifiedOld = await verifyTicketStep();
        if (verifiedOld === null) {
          busy.value = false;
          return;
        }

        const ok = await unifyPassword(props.rootId, verifiedOld, newPassword.value);
        if (!ok) {
          busy.value = false;
          return;
        }

        // F6 blob 删旧 + 重录（非阻塞）
        biometricRebindAfterUnify(props.rootId, newPassword.value).catch((err) => {
          console.warn('[PasswordUnifyPanel] biometric rebind failed:', err);
        });

        ElMessage.success('密码已统一，请用新密码登录');
        emit('done');
      } finally {
        busy.value = false;
      }
    }

    return {
      state,
      oldPassword,
      newPassword,
      confirmPassword,
      busy,
      message,
      oldInput,
      bannerText,
      canSubmit,
      submit,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/login.css"></style>
