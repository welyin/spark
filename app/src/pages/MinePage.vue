<!-- 我的页（个人设置）四栏架构：rail | 二级导航(280px) | 列表/概览(280px) | 详情/编辑(自适应)。
     MinePage 只做编排（登录门控 / P2P 信息 / 菜单切换），每个模块的第三、四栏为
     components/mine/ 下的独立组件（以 fragment 同时渲染两栏），编辑全部内联在第四栏。
     第三、四栏按 2:3 弹性分配剩余宽度（各最小 280px）；窗口最小宽度 904px（tauri 配置 960px 已覆盖）。
     组织空间下头像入口只显示「组织身份」模块，不展示个人设置菜单项；
     头部身份在组织空间显示组织头像/昵称，个人空间显示个人资料 -->
<template>
  <section class="mine-page">
    <!-- 未登录：登录/注册引导 -->
    <div v-if="!rootStatus.initialized || !rootStatus.unlocked" class="mine-plain">
      <header class="page-header">
        <div class="page-header-main mine-header-main">
          <div>
            <p class="eyebrow">个人设置</p>
            <h1>我的</h1>
            <p class="lede">账号登录前不会显示主界面，先完成 RootID 注册 / 登录。</p>
          </div>
        </div>
      </header>
      <RootAuthCenter @update-auth-state="syncAuthState" />
    </div>

    <template v-else>
      <!-- 第二栏：用户信息 + 功能菜单；移动端（波次 2）为栈1 整页，点开模块整页切换 -->
      <div v-if="!isMobileLayout || mobileFrame.page === 'root'" class="mine-menu">
        <header class="mine-menu-header">
          <UserAvatar
            :root-id="headerSource.seed"
            :nickname="headerSource.name"
            :avatar="headerSource.image"
            :size="44"
          />
          <div class="mine-menu-user">
            <b>{{ headerSource.name }}</b>
            <span>{{ headerSubtitle }}</span>
          </div>
        </header>

        <nav class="mine-menu-list">
          <button
            v-for="item in menuItems"
            :key="item.key"
            type="button"
            class="mine-menu-item"
            :class="{ active: activeMenu === item.key }"
            @click="onSelectMenu(item.key)"
          >
            <el-icon class="mine-menu-icon" :size="17"><component :is="item.icon" /></el-icon>
            <span class="mine-menu-label">{{ item.label }}</span>
          </button>
        </nav>
      </div>

      <!-- 移动端：栈2 模块页顶部返回栏（桌面端不渲染） -->
      <MobileBackBar v-if="isMobileLayout && mobileFrame.page === 'module'" :title="activeMenuLabel" @back="onMobileBack" />

      <!-- 第三、四栏：当前菜单对应模块（默认「我的资料」），各模块自带列表栏与详情栏；
           移动端仅栈2（模块页）渲染，栏位纵排见 mine.css 波次 2 媒体查询 -->
      <ProfileModule
        v-if="showModules && activeMenu === 'profile'"
        :root-id="rootStatus.rootId ?? ''"
        :nickname="rootStatus.nickname ?? ''"
        :avatar="rootStatus.avatar ?? ''"
        @profile-updated="onProfileUpdated"
      />
      <MyCardModule v-else-if="showModules && activeMenu === 'card'" />
      <BackupModule v-else-if="showModules && activeMenu === 'backup'" :root-id="rootStatus.rootId" />
      <!-- 组织身份（仅组织空间出现在菜单中） -->
      <OrgIdentityModule v-else-if="showModules && activeMenu === 'org'" />
      <!-- 朋友权限（个人：仅聊天+黑名单）/ 成员权限（组织：仅黑名单） -->
      <PermissionModule v-else-if="showModules && activeMenu === 'permission'" :mode="currentSpace.type === 'org' ? 'org' : 'personal'" />
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch, type Component } from 'vue';
import { ElMessage } from 'element-plus';
import { Key, Lock, OfficeBuilding, Postcard, User } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId } from '../stores/current-space';
import { getOrgIdentity } from '../stores/org-identity';
import { isMobileLayout } from '../stores/ui-layout';
import { currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import type { RootStatusDto as RootStatus } from '../api';
import { orgIdentityAvatarSource, personalAvatarSource } from '../stores/avatar-sources';
import UserAvatar from '../components/UserAvatar.vue';
import MobileBackBar from '../components/MobileBackBar.vue';
import ProfileModule from '../components/mine/ProfileModule.vue';
import MyCardModule from '../components/mine/MyCardModule.vue';
import BackupModule from '../components/mine/BackupModule.vue';
import OrgIdentityModule from '../components/mine/OrgIdentityModule.vue';
import PermissionModule from '../components/mine/PermissionModule.vue';
import RootAuthCenter from './auth/RootAuthCenter.vue';

type MenuKey = 'profile' | 'card' | 'backup' | 'org' | 'permission';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'mine';

export default defineComponent({
  name: 'MinePage',
  components: {
    UserAvatar,
    MobileBackBar,
    ProfileModule,
    MyCardModule,
    BackupModule,
    OrgIdentityModule,
    PermissionModule,
    RootAuthCenter
  },
  emits: ['profile-updated'],
  setup(_, { emit }) {
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });
    // 组织空间下头像入口只承载「组织身份」及其编辑，默认即选中
    const activeMenu = ref<MenuKey>(currentSpace.value.type === 'org' ? 'org' : 'profile');

    const menuItems = computed<Array<{ key: MenuKey; label: string; icon: Component }>>(() => {
      // 组织空间：不显示个人设置菜单，仅保留组织身份与成员权限
      if (currentSpace.value.type === 'org') {
        return [
          { key: 'org', label: '组织身份', icon: OfficeBuilding },
          { key: 'permission', label: '成员权限', icon: Key }
        ];
      }
      // 网络状态/设备管理在系统设置中，此处不再重复
      return [
        { key: 'profile', label: '我的资料', icon: User },
        { key: 'card', label: '我的名片', icon: Postcard },
        { key: 'permission', label: '朋友权限', icon: Key },
        { key: 'backup', label: '账号备份', icon: Lock }
      ];
    });

    // 空间切换时同步选中项：进组织空间锁定到「组织身份」，回个人空间恢复默认面板；
    // 移动端同步回栈底（菜单项集合随空间变化）
    watch(
      () => currentSpace.value,
      (space) => {
        if (space.type === 'org') {
          activeMenu.value = 'org';
        } else if (activeMenu.value === 'org') {
          activeMenu.value = 'profile';
        }
        resetStack(MOBILE_TAB);
      }
    );

    // ------------------------------------------------------------------
    // 移动端导航栈（波次 2）：栈1 功能菜单 → 栈2 模块页；桌面端以下逻辑均不触发
    // ------------------------------------------------------------------
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));
    /** 模块区渲染条件：桌面常驻；移动端仅栈2（模块页） */
    const showModules = computed(() => !isMobileLayout.value || mobileFrame.value.page === 'module');

    /** 菜单选中：桌面切右栏模块；移动端压入模块页栈帧（整页） */
    const onSelectMenu = (key: MenuKey) => {
      activeMenu.value = key;
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'module', { key });
      }
    };

    /** 返回栏：弹出栈顶回功能菜单（栈1） */
    const onMobileBack = () => popPage(MOBILE_TAB);

    /** 返回栏标题：当前模块名 */
    const activeMenuLabel = computed(
      () => menuItems.value.find((item) => item.key === activeMenu.value)?.label ?? '我的'
    );

    // 栈顶帧变化（重进 tab 按栈恢复 / 重按 tab 复位）时同步选中模块
    watch(
      [mobileFrame, isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        const key = frame.params?.key as MenuKey | undefined;
        if (frame.page === 'module' && menuItems.value.some((item) => item.key === key)) {
          activeMenu.value = key as MenuKey;
        }
      },
      { immediate: true }
    );

    // 头部身份：组织空间且未开「使用个人身份」时显示组织身份（+「组织身份」副标题），
    // 否则显示个人资料（与 UserAvatarMenu 同口径）；取数统一走 avatar-sources
    const isOrgSpace = computed(() => currentSpace.value.type === 'org');
    const isOrgIdentity = computed(
      () => isOrgSpace.value && !getOrgIdentity(currentSpaceOrgId.value).usePersonalIdentity
    );
    const headerSource = computed(() =>
      isOrgIdentity.value ? orgIdentityAvatarSource(currentSpaceOrgId.value) : personalAvatarSource()
    );
    const headerSubtitle = computed(() => (isOrgIdentity.value ? '组织身份' : '个人设置'));

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
    };

    const syncAuthState = (status: RootStatus) => {
      rootStatus.value = status;
    };

    const onProfileUpdated = (result: { nickname: string | null; avatar: string | null }) => {
      rootStatus.value = { ...rootStatus.value, nickname: result.nickname, avatar: result.avatar };
      // 通知外壳刷新 rail 头像与空间切换器的个人空间头像（与 SettingsPage 同口径）
      emit('profile-updated');
    };

    onMounted(async () => {
      try {
        await refreshStatus();
      } catch (error) {
        ElMessage.error(`读取状态失败：${error}`);
      }
    });

    return {
      rootStatus,
      activeMenu,
      menuItems,
      currentSpace,
      headerSource,
      headerSubtitle,
      syncAuthState,
      onProfileUpdated,
      isMobileLayout,
      mobileFrame,
      showModules,
      activeMenuLabel,
      onSelectMenu,
      onMobileBack
    };
  }
});
</script>

<!-- 非 scoped（同 settings.css）：.mine-list / .mine-detail 等栏位样式由各模块组件共用 -->
<style src="../styles/pages/mine.css"></style>
