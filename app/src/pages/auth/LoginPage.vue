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
        <el-input ref="passwordInput" v-model="password" type="password" show-password placeholder="输入密码" :disabled="busy" @keydown.enter.prevent="submit" />
      </el-form-item>
      <el-button class="submit-btn" type="primary" native-type="button" :loading="busy" :disabled="busy" @click="submit">登录</el-button>
    </el-form>
    <div class="entry-link">
      <el-button link type="primary" :disabled="busy" @click="emit('switch')">切换用户</el-button>
    </div>

    <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref, watch } from 'vue';
import UserAvatar from '../../components/UserAvatar.vue';

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
      default: null
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
  emits: ['login', 'switch'],
  setup(props, { emit }) {
    const password = ref('');
    const message = ref('');
    const passwordInput = ref<{ focus: () => void } | null>(null);
    // 打开登录页即聚焦密码框（组件按 authMode 条件渲染，每次出现都会触发 onMounted）
    onMounted(() => {
      passwordInput.value?.focus();
    });
    // 重入守卫：回车可能同时触发原生 form 提交与 keyup.enter，busy 是父级 prop（异步回传），
    // 仅靠 props.busy 拦不住同一帧内的第二次触发；父级 busy 结束后复位
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

      if (props.busy || submitting.value) {
        return;
      }

      message.value = '';
      submitting.value = true;
      emit('login', password.value);
    };

    return {
      password,
      message,
      passwordInput,
      submit,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/login.css"></style>
