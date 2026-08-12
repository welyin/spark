<template>
  <section class="auth-panel">
    <h2 class="auth-title">用户登录</h2>
    <p class="hint">登录会解锁 RootID，用于签名与授权。</p>

    <div v-if="rootId" class="login-profile">
      <UserAvatar :root-id="rootId" :nickname="nickname" :avatar="avatar" :size="48" />
      <div class="login-profile-name">{{ nickname || '未命名用户' }}</div>
    </div>

    <!-- 回车/点击都显式触发 submit，完全不经过原生表单提交：keydown.enter.prevent 在 keydown 阶段
         掐掉 webview 隐式提交（其默认动作里的表单/密码自动填充处理会同步卡主线程，蒙版画不出来），
         按钮用 native-type="button" + @click（不走按钮激活提交）；@submit.prevent 纯兜底防刷新 -->
    <el-form label-position="top" class="auth-form" @submit.prevent>
      <el-form-item label="登录密码">
        <el-input ref="passwordInput" v-model="password" type="password" show-password placeholder="输入密码" :disabled="busy || bioBusy" @keydown.enter.prevent="submit" />
      </el-form-item>
      <el-button class="submit-btn" type="primary" native-type="button" :loading="busy || bioBusy" :disabled="busy || bioBusy" @click="submit">登录</el-button>
      <div v-if="showBioRetry" class="entry-link">
        <el-button link type="primary" :disabled="bioBusy" @click="manualBiometricUnlock">使用指纹/人脸</el-button>
      </div>
    </el-form>
    <div class="entry-link">
      <el-button link type="primary" :disabled="busy || bioBusy" @click="emit('switch')">切换账号</el-button>
    </div>
    <div class="entry-link">
      <el-button link type="info" :disabled="busy || bioBusy" @click="emit('recover')">忘记密码？</el-button>
    </div>

    <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref, watch } from 'vue';
import UserAvatar from '../../components/UserAvatar.vue';
import { isMobileLayout } from '../../stores/ui-layout';
import { biometricCheck } from '../../utils/biometric';
import { isBiometricUnlockEnabled } from '../../utils/biometric-setting';
import { runBiometricRitual } from '../../utils/biometric-ritual';

export default defineComponent({
  name: 'LoginPage',
  components: {
    UserAvatar
  },
  props: {
    busy: {
      type: Boolean,
      default: false
    },
    rootId: {
      type: String,
      default: ''
    },
    nickname: {
      type: String,
      default: ''
    },
    avatar: {
      type: String,
      default: ''
    }
  },
  emits: ['login', 'switch', 'recover'],
  setup(props, { emit }) {
    const password = ref('');
    const message = ref('');
    const passwordInput = ref<{ focus: () => void } | null>(null);
    const bioBusy = ref(false);
    const showBioRetry = ref(false);

    const submitting = ref(false);
    watch(
      () => props.busy,
      (value) => {
        if (!value) {
          submitting.value = false;
        }
      }
    );

    const submit = async () => {
      if (!password.value) {
        message.value = '请输入密码';
        return;
      }

      if (props.busy || submitting.value || bioBusy.value) {
        return;
      }

      message.value = '';
      submitting.value = true;
      emit('login', password.value);
    };

    async function tryBiometricUnlock(auto = false) {
      if (!props.rootId || !isMobileLayout.value) return;
      bioBusy.value = true;
      message.value = '';
      try {
        const result = await runBiometricRitual(props.rootId);
        message.value = result.message;
        if (result.password === null) {
          showBioRetry.value = result.showRetry;
          passwordInput.value?.focus();
          return;
        }
        // 第三参 bioSourced=true：密码来自生物识别认证解密（已当场通过验证），
        // RootGate 据此跳过 M4 口令保鲜重刷，避免登录时连续两次系统指纹验证。
        emit('login', result.password, true);
      } finally {
        bioBusy.value = false;
      }
    }

    function manualBiometricUnlock() {
      tryBiometricUnlock(false);
    }

    onMounted(async () => {
      passwordInput.value?.focus();
      if (!props.rootId || !isMobileLayout.value) return;
      if (!isBiometricUnlockEnabled()) return;
      try {
        const status = await biometricCheck();
        if (status.hasSecret) {
          showBioRetry.value = true;
          await tryBiometricUnlock(true);
        } else {
          showBioRetry.value = false;
        }
      } catch {
        showBioRetry.value = false;
      }
    });

    return {
      password,
      message,
      passwordInput,
      bioBusy,
      showBioRetry,
      submit,
      manualBiometricUnlock,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/login.css"></style>
