<template>
  <section class="root-gate">
    <App v-if="isPluginWindow || showApp" />

    <div v-else class="gate-wrap" :class="{ 'gate-busy': authBusy }" v-loading="authBusy" element-loading-text="正在登录...">
      <header class="brand">
        <img class="brand-logo" :src="sparkLogo" alt="星火" />
        <h1 class="brand-name">星火</h1>
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
            :root-id="rootStatus.rootId ?? ''"
            :nickname="rootStatus.nickname ?? ''"
            :avatar="rootStatus.avatar ?? ''"
            @login="handleLogin"
            @switch="authMode = 'switch'"
          />
          <SwitchUserPage
            v-else-if="authMode === 'switch'"
            @select="handleSwitchSelect"
            @register="authMode = 'register'"
            @recover="authMode = 'recover'"
            @back="authMode = 'login'"
          />
          <RegisterPage
            v-else-if="authMode === 'register'"
            show-back
            @registered="handleRegistered"
            @recover="authMode = 'recover'"
            @back="authMode = 'login'"
          />
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
import { defineComponent, nextTick, onMounted, onUnmounted, ref } from 'vue';
import App from './App.vue';
import sparkLogo from './assets/spark-logo.png';
import type { RootStatusDto as RootStatus } from './api';
import RegisterPage from './pages/auth/RegisterPage.vue';
import LoginPage from './pages/auth/LoginPage.vue';
import RecoverPage from './pages/auth/RecoverPage.vue';
import SwitchUserPage from './pages/auth/SwitchUserPage.vue';
import { errorMessage } from './utils/ipc';

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
      // 主动收起键盘并复位滚动：键盘收起过程中 WebView 可能分多帧还原滚动位置，
      // 若不先复位，gate-wrap 停留在被键盘顶起的位置，loading 蒙版（absolute 于滚动容器内）
      // 随内容被卷走，看起来像"卡住"；延迟再兜底一次末帧还原
      (document.activeElement as HTMLElement | null)?.blur?.();
      resetGateScroll();
      setTimeout(resetGateScroll, 300);
      // 先让 Vue 渲染并绘制出蒙版，再开始解锁（避免事件回合内的同步工作挤掉蒙版绘制）
      await nextTick();
      await new Promise((resolve) => setTimeout(resolve, 0));
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

    // 软键盘适配（Android 前端改造）：键盘弹出时 WebView 可视区收缩（visualViewport 高度变小），
    // 键盘收起时恢复。100dvh 布局已跟随高度自动收缩；此兜底处理滚动位置残留与 dvh 不支持情况：
    // 键盘收起时强制回滚到顶部，避免界面停留在键盘弹出时的位置。
    // 注意：移动端登录页禁止页面级滚动（.root-gate overflow:hidden），真正的滚动容器是
    // .gate-wrap（overflow-y:auto）——复位对象必须是它，window/documentElement 滚动不生效。
    // 收起判定与"历史最高可视高度"比较（≈无键盘高度）：部分 WebView 键盘收起动画分多帧派发
    // resize、每帧增量不足 40px，与上一帧比较会永远不触发复位。
    let maxViewportHeight = 0;
    let keyboardOpen = false;
    const resetGateScroll = () => {
      window.scrollTo(0, 0);
      document.documentElement.scrollTop = 0;
      document.querySelector('.gate-wrap')?.scrollTo({ top: 0 });
    };
    // 横竖屏切换（Android 未锁竖屏，见 AndroidManifest configChanges）：基准高度随朝向变化，
    // 沿用旧基准会把切换后的正常高度差误判为键盘弹出/收起；切换时重置基准与键盘态，
    // 由随后的 resize 在新朝向下重新积累基准
    const onOrientationChange = () => {
      maxViewportHeight = 0;
      keyboardOpen = false;
    };
    const onViewportResize = () => {
      if (!window.visualViewport) {
        return;
      }
      const vh = window.visualViewport.height;
      if (vh > maxViewportHeight) {
        maxViewportHeight = vh;
      }
      // 较历史最高值矮 120px 以上视为键盘弹出（移动键盘通常 ≥200px）
      if (maxViewportHeight - vh > 120) {
        keyboardOpen = true;
        return;
      }
      // 从"键盘弹出（矮）"回到"接近最高值"：界面复位回顶部；
      // 部分 WebView 在收起动画末帧才还原滚动位置，延迟再兜底一次
      if (keyboardOpen) {
        keyboardOpen = false;
        resetGateScroll();
        setTimeout(resetGateScroll, 150);
      }
    };

    onMounted(async () => {
      if (isPluginWindow.value) {
        showApp.value = true;
        return;
      }
      await refreshStatus();
      // 播种基准高度：挂载时即取当前可视高度为历史最高值，避免首个键盘周期
      //（弹出前未积累基准）丢失收起时的复位判定
      maxViewportHeight = window.visualViewport?.height ?? window.innerHeight;
      // 监听可视区高度变化（键盘弹出/收起）；旧 WebView 无 visualViewport 时靠 resize 兜底
      window.visualViewport?.addEventListener('resize', onViewportResize);
      window.addEventListener('resize', onViewportResize);
      // 横竖屏切换重置基准（screen.orientation 为主，orientationchange 兜底旧 WebView）
      window.screen.orientation?.addEventListener('change', onOrientationChange);
      window.addEventListener('orientationchange', onOrientationChange);
    });

    onUnmounted(() => {
      window.visualViewport?.removeEventListener('resize', onViewportResize);
      window.removeEventListener('resize', onViewportResize);
      window.screen.orientation?.removeEventListener('change', onOrientationChange);
      window.removeEventListener('orientationchange', onOrientationChange);
    });

    return {
      isPluginWindow,
      sparkLogo,
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
