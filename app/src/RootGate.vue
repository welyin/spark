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
          <RegisterPage v-if="authMode === 'register'" @registered="handleRegistered" @add="authMode = 'add'" @recover="authMode = 'recover'" />
          <AddAccountPage v-else-if="authMode === 'add'" ref="addAccountRef" @recovered="handleRecovered" @recover="authMode = 'recover'" @back="authMode = 'register'" />
          <RecoverPage v-else :root-id="rootStatus.rootId ?? ''" @recovered="handleRecovered" @back="authMode = 'register'" />
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
            @recover="authMode = 'recover'"
          />
          <SwitchUserPage
            v-else-if="authMode === 'switch'"
            @select="handleSwitchSelect"
            @register="authMode = 'register'"
            @add="authMode = 'add'"
            @recover="authMode = 'recover'"
            @back="authMode = 'login'"
          />
          <RegisterPage
            v-else-if="authMode === 'register'"
            show-back
            @registered="handleRegistered"
            @add="authMode = 'add'"
            @recover="authMode = 'recover'"
            @back="authMode = 'login'"
          />
          <AddAccountPage
            v-else-if="authMode === 'add'"
            ref="addAccountRef"
            @recovered="handleRecovered"
            @recover="authMode = 'recover'"
            @back="authMode = 'switch'"
          />
          <RecoverPage
            v-else-if="authMode === 'recover'"
            back-label="返回用户列表"
            :root-id="rootStatus.rootId ?? ''"
            @recovered="handleRecovered"
            @back="authMode = 'switch'"
          />
          <PasswordUnifyPanel
            v-else-if="authMode === 'unify'"
            :root-id="rootStatus.rootId ?? ''"
            @back="authMode = 'login'"
            @done="handleUnifyDone"
          />
        </template>

        <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon class="gate-message" />
        <!-- 乙+校验器：登录后若其他设备改了密码，常驻提示条引导统一新密码 -->
        <div v-if="pendingUnifyRef" class="unify-banner">
          <span>{{ pendingUnifyRef.reason === 'password_reset' ? '⚠️ 密码已被重置：' : '密码已在其他设备上修改：' }}请使用新密码统一登录凭据。</span>
          <el-button link type="primary" @click="authMode = 'unify'">统一为新密码</el-button>
        </div>
      </div>
    </div>
  </section>
</template>

<script lang="ts">
import { defineComponent, nextTick, onMounted, onUnmounted, ref } from 'vue';
import { onBackButtonPress } from '@tauri-apps/api/app';
import App from './App.vue';
import sparkLogo from './assets/spark-logo.png';
import type { RootStatusDto as RootStatus } from './api';
import RegisterPage from './pages/auth/RegisterPage.vue';
import LoginPage from './pages/auth/LoginPage.vue';
import RecoverPage from './pages/auth/RecoverPage.vue';
import SwitchUserPage from './pages/auth/SwitchUserPage.vue';
import AddAccountPage from './pages/auth/AddAccountPage.vue';
import PasswordUnifyPanel from './pages/auth/PasswordUnifyPanel.vue';
import { errorMessage } from './utils/ipc';
import { isAutoLockExpired, touchLastActiveAt } from './utils/auto-lock';
import { biometricStorePassword, biometricErrorMessage } from './utils/biometric';
import { isBiometricUnlockEnabled } from './utils/biometric-setting';
import { promptBiometricBind } from './utils/biometric-prompt';
import { isMobileLayout } from './stores/ui-layout';
import { hydratePasswordUnify, pendingUnifyRef, clearPasswordUnifyPending } from './stores/password-unify';
import { resetContactsCache } from './mock/contacts/store';
import { resetMessagesCache } from './stores/messages';

type AuthMode = 'login' | 'switch' | 'register' | 'recover' | 'add' | 'unify';

export default defineComponent({
  name: 'RootGate',
  components: {
    App,
    RegisterPage,
    LoginPage,
    RecoverPage,
    SwitchUserPage,
    AddAccountPage,
    PasswordUnifyPanel
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
    // AddAccountPage 实例 ref（首装/已有账号分支共用，同时间只渲染一个）：
    // 系统返回键需查询其摄像头状态、外部触发关闭（全屏覆盖层语义等同覆盖层，按返回先关摄像头）
    const addAccountRef = ref<InstanceType<typeof AddAccountPage> | null>(null);

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
      statusLoaded.value = true;
      if (rootStatus.value.initialized && rootStatus.value.unlocked) {
        // N 天未使用自动锁定（§5）：超时且当前会被视为已解锁 → 立即锁回登录页，
        // 拒绝进入已解锁主界面（现状无生物识别自动解锁，本检查主要是语义占位，
        // 但保证超时后不落已解锁态）
        if (isAutoLockExpired()) {
          await window.electronAPI.rootIdentity.lock();
          rootStatus.value = { ...rootStatus.value, unlocked: false };
          showApp.value = false;
          authMode.value = 'login';
          message.value = '已长时间未使用，请重新输入密码';
          return;
        }
        showApp.value = true;
      } else if (rootStatus.value.initialized && authMode.value === 'register') {
        // 已有账号但未登录时默认落在登录页（首装无账号时落在注册页）
        authMode.value = 'login';
      }
    };

    const handleRegistered = async (rootId: string) => {
      message.value = `注册成功，RootID=${rootId}`;
      // 新注册即活跃：记录活跃时间，避免自动锁定误判（§5）
      touchLastActiveAt();
      await refreshStatus();
    };

    const handleRecovered = async (rootId: string) => {
      message.value = `账号已恢复，RootID=${rootId}`;
      touchLastActiveAt();
      await refreshStatus();
    };

    const handleLogin = async (password: string, bioSourced = false) => {
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
        // 登录成功即活跃：刷新自动锁定的最近活跃时间（§5）
        touchLastActiveAt();
        // M4 口令保鲜：设置项开启且是「手动输密码」登录时，用刚验证过的密码重刷生物识别凭据
        // （覆盖改密后旧 blob 失配死路）。bioSourced（生物识别解锁）拿到的密码即 blob 自身，
        // 已当场通过认证，重刷只会再多弹一次系统指纹验证 → 跳过。
        if (isBiometricUnlockEnabled() && !bioSourced) {
          try {
            await biometricStorePassword(password);
          } catch (err) {
            console.warn('[RootGate] biometricStorePassword failed:', biometricErrorMessage(err));
          }
        }
        // 移动端首次登录引导绑定生物识别（弹窗是非阻塞的，不等待它完成）
        if (isMobileLayout.value && !isBiometricUnlockEnabled()) {
          promptBiometricBind(password).catch((err) => {
            console.warn('[RootGate] promptBiometricBind failed:', err);
          });
        }
        // 乙+校验器：登录成功后水合 pendingUnify，常驻条将提示用户统一新密码
        await hydratePasswordUnify(result.rootId);
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
        // 身份切换：清空窗口会话级缓存，避免旧账号数据带进新会话
        resetContactsCache();
        resetMessagesCache();
        authMode.value = 'login';
        message.value = '';
        await refreshStatus();
      } catch (error) {
        message.value = `切换失败：${errorMessage(error)}`;
      }
    };

    const handleUnifyDone = () => {
      authMode.value = 'login';
      clearPasswordUnifyPending();
      message.value = '密码统一完成，请用新密码登录';
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

    // 系统返回键辅助：查询/关闭 AddAccountPage 摄像头（全屏覆盖层语义）
    const cameraShouldStop = (): boolean => {
      const inst = addAccountRef.value;
      if (!inst || authMode.value !== 'add') {
        return false;
      }
      return inst.isCameraActive();
    };
    const stopCameraExternally = () => {
      addAccountRef.value?.stopCamera();
    };

    // 系统返回键辅助：按 authMode 状态机回退到上一页（与各页面"返回"按钮语义一致）。
    // 返回 true 表示已处理回退；false 表示落到分支根页（首装=register / 已有账号=login），
    // 由调用方决定是否退出应用。
    const backFromCurrentMode = (): boolean => {
      const initialized = rootStatus.value.initialized;
      // 首装场景（无账号）：register(根) ↔ add ↔ recover
      if (!initialized) {
        switch (authMode.value) {
          case 'add':
            authMode.value = 'register';
            return true;
          case 'recover':
            authMode.value = 'register';
            return true;
          default:
            return false; // register 是根页
        }
      }
      // 已有账号未登录场景：login(根) ↔ switch ↔ register ↔ add ↔ recover
      switch (authMode.value) {
        case 'switch':
          authMode.value = 'login';
          return true;
        case 'register':
          authMode.value = 'login';
          return true;
        case 'add':
          authMode.value = 'switch';
          return true;
        case 'recover':
          // recover 在已有账号场景下入口可能来自 login/switch/register/add，回退到 switch
          // （recover 的 @back 在已有账号场景统一指向 switch，见 RecoverPage 模板）
          authMode.value = 'switch';
          return true;
        default:
          return false; // login 是根页
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

      // Android 系统返回键（认证态）：未解锁时按 authMode 状态机回退到上一页，
      // 落到分支根页（首装=register / 已有账号=login）再按退出应用。
      // 已解锁态（showApp=true）时 App.vue 的 handler 已接管，此处 return 不做事——
      // 避免与 App.vue handler 重复处理（onBackButtonPress 是事件监听，所有 handler 都触发）。
      // 桌面端无此事件，注册静默失败。
      onBackButtonPress(() => {
        if (showApp.value) {
          return;
        }
        if (cameraShouldStop()) {
          stopCameraExternally();
          return;
        }
        const prev = backFromCurrentMode();
        if (prev) {
          return;
        }
        // 落到分支根页：退出应用（与 App.vue 一级页语义一致，走 system 域收敛点）
        window.electronAPI?.system.exitApp().catch(() => {});
      }).catch(() => {
        // 桌面端 app 插件无 register_listener，注册静默失败
      });
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
      addAccountRef,
      pendingUnifyRef,
      handleRegistered,
      handleRecovered,
      handleLogin,
      handleSwitchSelect,
      handleUnifyDone
    };
  }
});
</script>

<style scoped src="./styles/root-gate.css"></style>
