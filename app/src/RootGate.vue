<template>
  <section class="root-gate">
    <App v-if="isPluginWindow || showApp" />

    <div v-else class="gate-wrap" v-loading="authBusy" element-loading-text="正在登录...">
      <header class="brand">
        <span class="brand-logo">S</span>
        <h1 class="brand-name">Spark</h1>
        <p class="brand-slogan">去中心化的组织协作网络</p>
      </header>

      <div class="gate-panel">
        <p v-if="!statusLoaded" class="desc gate-loading">正在读取账号状态…</p>

        <template v-else-if="!rootStatus.initialized">
          <RegisterPage v-if="authMode !== 'recover'" @registered="handleRegistered" @recover="authMode = 'recover'" />
          <RecoverPage v-else @recovered="handleRecovered" @back="authMode = 'register'" />
        </template>

        <template v-else-if="!rootStatus.unlocked">
          <LoginPage
            v-if="authMode === 'login'"
            :busy="authBusy"
            :root-id="rootStatus.rootId"
            :nickname="rootStatus.nickname ?? ''"
            :avatar="rootStatus.avatar ?? ''"
            @login="handleLogin"
            @switch="authMode = 'switch'"
          />
          <SwitchUserPage
            v-else-if="authMode === 'switch'"
            @select="handleSwitchSelect"
            @register="authMode = 'register'"
            @back="authMode = 'login'"
          />
          <RegisterPage v-else-if="authMode === 'register'" @registered="handleRegistered" @recover="authMode = 'recover'" />
          <RecoverPage v-else back-label="返回用户列表" @recovered="handleRecovered" @back="authMode = 'switch'" />
        </template>

        <div v-else class="ready-actions">
          <el-button type="primary" @click="showApp = true">进入主界面</el-button>
          <el-button type="danger" plain @click="handleLogout">退出登录</el-button>
        </div>

        <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="gate-message" />
      </div>
    </div>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import App from './App.vue';
import RegisterPage from './pages/auth/RegisterPage.vue';
import LoginPage from './pages/auth/LoginPage.vue';
import RecoverPage from './pages/auth/RecoverPage.vue';
import SwitchUserPage from './pages/auth/SwitchUserPage.vue';
import { errorMessage } from './utils/ipc';

type RootStatus = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
  nickname: string | null;
  avatar: string | null;
};

type AuthMode = 'login' | 'switch' | 'register' | 'recover';

export default defineComponent({
  name: 'RootGate',
  components: {
    App,
    RegisterPage,
    LoginPage,
    RecoverPage,
    SwitchUserPage
  },
  setup() {
    const search = new URLSearchParams(window.location.search);
    const isPluginWindow = ref(Boolean(search.get('pluginDomain')));

    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });
    const showApp = ref(false);
    const authBusy = ref(false);
    const message = ref('');
    const authMode = ref<AuthMode>('register');
    const statusLoaded = ref(false);

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
      statusLoaded.value = true;
      if (rootStatus.value.initialized && rootStatus.value.unlocked) {
        showApp.value = true;
      } else if (rootStatus.value.initialized && authMode.value === 'register') {
        // 已有账号但未登录时默认落在登录页（首装无账号时落在注册页）
        authMode.value = 'login';
      }
    };

    const handleRegistered = async (rootId: string) => {
      message.value = `注册成功，RootID=${rootId}`;
      await refreshStatus();
    };

    const handleRecovered = async (rootId: string) => {
      message.value = `账号已恢复，RootID=${rootId}`;
      await refreshStatus();
    };

    const handleLogin = async (password: string) => {
      authBusy.value = true;
      try {
        const result = await window.electronAPI.rootIdentity.unlock(password);
        message.value = `登录成功，RootID=${result.rootId}`;
        showApp.value = true;
        void refreshStatus();
      } catch (error) {
        message.value = `登录失败：${errorMessage(error)}`;
      } finally {
        authBusy.value = false;
      }
    };

    const handleSwitchSelect = async (rootId: string) => {
      try {
        await window.electronAPI.rootIdentity.setActive(rootId);
        authMode.value = 'login';
        message.value = '';
        await refreshStatus();
      } catch (error) {
        message.value = `切换失败：${errorMessage(error)}`;
      }
    };

    const handleLogout = async () => {
      try {
        await window.electronAPI.rootIdentity.lock();
        showApp.value = false;
        authMode.value = 'login';
        await refreshStatus();
      } catch (error) {
        message.value = `退出失败：${errorMessage(error)}`;
      }
    };

    onMounted(async () => {
      if (isPluginWindow.value) {
        showApp.value = true;
        return;
      }
      await refreshStatus();
    });

    return {
      isPluginWindow,
      rootStatus,
      showApp,
      authBusy,
      message,
      authMode,
      statusLoaded,
      handleRegistered,
      handleRecovered,
      handleLogin,
      handleSwitchSelect,
      handleLogout
    };
  }
});
</script>

<style scoped src="./styles/root-gate.css"></style>
