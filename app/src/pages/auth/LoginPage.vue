<template>
  <section class="auth-panel">
    <h2 class="auth-title">用户登录</h2>
    <p class="hint">登录会解锁 RootID，用于签名与授权。</p>

    <div v-if="rootId" class="login-profile">
      <UserAvatar :root-id="rootId" :nickname="nickname" :avatar="avatar" :size="56" />
      <div class="login-profile-name">{{ nickname || '未命名用户' }}</div>
    </div>

    <el-form label-position="top" class="auth-form">
      <el-form-item label="登录密码">
        <el-input v-model="password" type="password" show-password placeholder="输入注册密码" :disabled="busy" @keyup.enter="submit" />
      </el-form-item>
    </el-form>

    <el-button class="submit-btn" type="primary" :loading="busy" :disabled="busy" @click="submit">解锁 RootID</el-button>
    <div class="entry-link">
      <el-button link type="primary" :disabled="busy" @click="emit('switch')">切换用户</el-button>
    </div>

    <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
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

    const submit = async () => {
      if (!password.value) {
        message.value = '请输入密码';
        return;
      }

      if (props.busy) {
        return;
      }

      message.value = '';
      emit('login', password.value);
    };

    return {
      password,
      message,
      submit,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/login.css"></style>
