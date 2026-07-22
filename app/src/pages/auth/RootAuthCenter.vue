<template>
  <section class="auth-center">
    <el-card shadow="never">
      <template #header>
        <h1>我的</h1>
      </template>
      <p class="lede">账号登录前不会显示主界面，先完成 RootID 注册 / 登录。</p>

      <el-descriptions :column="1" border class="status-grid">
        <el-descriptions-item label="是否已注册">{{ rootStatus.initialized ? '是' : '否' }}</el-descriptions-item>
        <el-descriptions-item label="是否已登录">{{ rootStatus.unlocked ? '是' : '否' }}</el-descriptions-item>
        <el-descriptions-item label="RootID">{{ rootStatus.rootId || '未创建' }}</el-descriptions-item>
      </el-descriptions>

      <div class="section-wrap">
        <p v-if="!statusLoaded" class="lede">正在读取账号状态…</p>

        <template v-else-if="!rootStatus.initialized">
          <RegisterPage v-if="authMode !== 'recover'" @registered="handleRegistered" @recover="authMode = 'recover'" />
          <RecoverPage v-else @recovered="handleRecovered" @back="authMode = 'register'" />
        </template>

        <template v-else-if="!rootStatus.unlocked">
          <LoginPage
            v-if="authMode === 'login'"
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

        <el-card v-else shadow="never" class="inner-card">
          <template #header>
            <h2>已登录</h2>
          </template>
          <div class="row">
            <el-button @click="openRootPage">RootID</el-button>
            <el-button type="danger" plain @click="handleLogout">退出登录</el-button>
          </div>
        </el-card>
      </div>

      <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="block-gap" />
    </el-card>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import RegisterPage from './RegisterPage.vue';
import LoginPage from './LoginPage.vue';
import RecoverPage from './RecoverPage.vue';
import SwitchUserPage from './SwitchUserPage.vue';
import { errorMessage } from '../../utils/ipc';

type RootStatus = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
  nickname: string | null;
  avatar: string | null;
};

type AuthMode = 'login' | 'switch' | 'register' | 'recover';

export default defineComponent({
  name: 'RootAuthCenter',
  components: {
    RegisterPage,
    LoginPage,
    RecoverPage,
    SwitchUserPage
  },
  emits: ['open-root-page', 'update-auth-state'],
  setup(_, { emit }) {
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });
    const message = ref('');
    const authMode = ref<AuthMode>('register');
    const statusLoaded = ref(false);

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
      statusLoaded.value = true;
      if (rootStatus.value.initialized && !rootStatus.value.unlocked && authMode.value === 'register') {
        authMode.value = 'login';
      }
      emit('update-auth-state', rootStatus.value);
    };

    const handleRegistered = async (rootId: string) => {
      message.value = `注册成功，RootID=${rootId}`;
      ElMessage.success('注册成功');
      await refreshStatus();
    };

    const handleRecovered = async (rootId: string) => {
      message.value = `账号已恢复，RootID=${rootId}`;
      ElMessage.success('账号已恢复');
      await refreshStatus();
    };

    const handleLogin = async (password: string) => {
      try {
        const result = await window.electronAPI.rootIdentity.unlock(password);
        message.value = `登录成功，RootID=${result.rootId}`;
        ElMessage.success('登录成功');
        await refreshStatus();
      } catch (error) {
        message.value = `登录失败：${errorMessage(error)}`;
        ElMessage.error(message.value);
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
        ElMessage.error(message.value);
      }
    };

    const handleLogout = async () => {
      try {
        await window.electronAPI.rootIdentity.lock();
        message.value = '已退出登录';
        ElMessage.success(message.value);
        authMode.value = 'login';
        await refreshStatus();
      } catch (error) {
        message.value = `退出失败：${errorMessage(error)}`;
        ElMessage.error(message.value);
      }
    };

    const openRootPage = () => {
      emit('open-root-page');
    };

    onMounted(async () => {
      await refreshStatus();
    });

    return {
      rootStatus,
      message,
      authMode,
      statusLoaded,
      handleRegistered,
      handleRecovered,
      handleLogin,
      handleSwitchSelect,
      handleLogout,
      openRootPage
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/root-auth-center.css"></style>
