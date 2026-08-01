<!-- 设置页：rail | 第二栏菜单 | 第三栏 | 第四栏（复用 mine.css 栏位类）。
     个人空间第二栏=个人设置 / 系统设置；组织空间第二栏=组织设置 / 个人设置 / 系统设置。
     「个人设置」下第三栏为四个模块（我的资料/我的名片/朋友权限/账号备份），点击模块在第四栏展开其列表，
     列表项的编辑页以抽屉打开（不占第五栏）；网络状态/设备管理在系统设置中；
     组织设置=当前空间组织的信息/成员/网关/公开/发现（ui-space-navbar §6）。
     登录门控由 RootGate 在应用入口统一处理，本页不再重复 -->
<template>
  <section class="mine-page settings-page">
    <!-- 第二栏：设置菜单；移动端（波次 2）为栈1 整页，点开分组整页切换 -->
    <div v-if="!isMobileLayout || mobileFrame.page === 'root'" class="mine-menu">
        <header class="mine-menu-header">
          <UserAvatar
            v-if="isPersonal"
            :root-id="headerAvatar.seed"
            :nickname="headerAvatar.name"
            :avatar="headerAvatar.image"
            :size="44"
          />
          <OrgAvatar v-else :org-id="currentSpaceOrgId" :name="currentOrgName" :size="44" />
          <div class="mine-menu-user">
            <b>设置</b>
            <span>{{ isPersonal ? `${headerAvatar.name}的个人空间` : currentOrgName }}</span>
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

      <!-- 移动端：栈深 >1 时顶部返回栏（桌面端不渲染） -->
      <MobileBackBar v-if="isMobileLayout && mobileCanBack" :title="mobileTitle" @back="onMobileBack" />

      <!-- 个人设置（个人/组织空间均有）：第三栏模块菜单 + 第四栏模块列表；个人空间另有系统设置。
           移动端：第三栏=栈2（分组页），第四栏模块=栈3（模块页） -->
      <template v-if="isPersonal || activeMenu === 'mine'">
        <!-- 第三栏：个人设置的模块菜单 -->
        <div v-if="activeMenu === 'mine' && (!isMobileLayout || mobileFrame.page === 'section')" class="mine-list">
          <h2 class="mine-list-title">个人设置</h2>
          <div class="mine-list-items">
            <button
              v-for="item in personalModules"
              :key="item.key"
              type="button"
              class="mine-list-item"
              :class="{ active: activeModule === item.key }"
              @click="onSelectModule(item.key)"
            >
              <el-icon class="mine-list-item-icon" :size="17"><component :is="item.icon" /></el-icon>
              <b class="settings-module-label">{{ item.label }}</b>
            </button>
          </div>
        </div>
        <SystemSettingsPanel v-else-if="activeMenu === 'system' && (!isMobileLayout || mobileFrame.page === 'section')" />

        <!-- 第四栏：选中模块的列表栏（模块编辑页以抽屉打开，不占第五栏） -->
        <template v-if="activeMenu === 'mine' && (!isMobileLayout || mobileFrame.page === 'module')">
          <ProfileModule
            v-if="activeModule === 'profile'"
            detail-mode="drawer"
            :root-id="rootStatus.rootId ?? ''"
            :nickname="rootStatus.nickname ?? ''"
            :avatar="rootStatus.avatar ?? ''"
            @profile-updated="onProfileUpdated"
          />
          <MyCardModule v-else-if="activeModule === 'card'" detail-mode="drawer" />
          <PermissionModule v-else-if="activeModule === 'permission'" detail-mode="drawer" mode="personal" />
          <BackupModule v-else-if="activeModule === 'backup'" detail-mode="drawer" :root-id="rootStatus.rootId" />
          <!-- 未选模块时的占位 -->
          <div v-else class="mine-detail settings-module-empty">
            <el-empty description="选择左侧模块查看" />
          </div>
        </template>
      </template>

      <!-- 组织空间：组织设置（当前空间组织的信息/成员/网关/公开/发现）/ 系统设置 -->
      <template v-else>
        <OrgSettingsPanel v-if="activeMenu === 'space' && (!isMobileLayout || mobileFrame.page === 'section')" />
        <SystemSettingsPanel v-else-if="activeMenu === 'system' && (!isMobileLayout || mobileFrame.page === 'section')" />
      </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch, type Component } from 'vue';
import { Key, Lock, OfficeBuilding, Postcard, Setting, User } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId } from '../stores/current-space';
import { spaceKeyOf } from '../mock/space-key';
import { nameOf, refreshOrganizations } from '../stores/org-membership';
import { isMobileLayout } from '../stores/ui-layout';
import { canBack, currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import type { RootStatusDto as RootStatus } from '../api';
import { personalAvatarSource } from '../stores/avatar-sources';
import UserAvatar from '../components/UserAvatar.vue';
import OrgAvatar from '../components/OrgAvatar.vue';
import MobileBackBar from '../components/MobileBackBar.vue';
import ProfileModule from '../components/mine/ProfileModule.vue';
import MyCardModule from '../components/mine/MyCardModule.vue';
import BackupModule from '../components/mine/BackupModule.vue';
import PermissionModule from '../components/mine/PermissionModule.vue';
import OrgSettingsPanel from '../components/org/OrgSettingsPanel.vue';
import SystemSettingsPanel from '../components/settings/SystemSettingsPanel.vue';

type MenuKey = 'mine' | 'space' | 'system';

/** 个人设置下的四个模块（第三栏菜单，点击后右侧展开；网络状态/设备管理已并入系统设置） */
type PersonalModuleKey = 'profile' | 'card' | 'permission' | 'backup';

/** 本页在导航栈中的 tab 键（设置不在底部 tab，经顶栏「⋯」进入，键与 App.vue activeTab 一致） */
const MOBILE_TAB = 'settings';

export default defineComponent({
  name: 'SettingsPage',
  components: {
    UserAvatar,
    OrgAvatar,
    MobileBackBar,
    ProfileModule,
    MyCardModule,
    BackupModule,
    PermissionModule,
    OrgSettingsPanel,
    SystemSettingsPanel
  },
  emits: ['profile-updated'],
  setup(_, { emit }) {
    const activeMenu = ref<MenuKey>(currentSpace.value.type === 'org' ? 'space' : 'mine');
    const activeModule = ref<PersonalModuleKey | null>(null);
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });

    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    // 空间 key（'personal' / 'org:<orgId>'）：组织 A→B 切换时 isPersonal 不变，watch 须以 spaceKey 为口径
    const spaceKey = computed(() => spaceKeyOf(currentSpace.value));
    const currentOrgName = computed(() => nameOf(currentSpaceOrgId.value) ?? '组织空间');
    // 页头个人头像：统一取数（stores/avatar-sources），与 rail/空间切换器同源
    const headerAvatar = computed(() => personalAvatarSource());

    const menuItems = computed<Array<{ key: MenuKey; label: string; icon: Component }>>(() => {
      if (isPersonal.value) {
        return [
          { key: 'mine', label: '个人设置', icon: User },
          { key: 'system', label: '系统设置', icon: Setting }
        ];
      }
      return [
        { key: 'space', label: '组织设置', icon: OfficeBuilding },
        { key: 'mine', label: '个人设置', icon: User },
        { key: 'system', label: '系统设置', icon: Setting }
      ];
    });

    // 个人设置的四个模块：第二栏「个人设置」下第三栏的菜单项
    const personalModules: Array<{ key: PersonalModuleKey; label: string; icon: Component }> = [
      { key: 'profile', label: '我的资料', icon: User },
      { key: 'card', label: '我的名片', icon: Postcard },
      { key: 'permission', label: '朋友权限', icon: Key },
      { key: 'backup', label: '账号备份', icon: Lock }
    ];

    // 空间切换（个人↔组织及组织 A→B）：菜单项集合变化，重置选中到各空间默认项，并清掉模块选中；移动端同步回栈底
    watch(spaceKey, () => {
      activeMenu.value = isPersonal.value ? 'mine' : 'space';
      activeModule.value = null;
      resetStack(MOBILE_TAB);
    });

    // ------------------------------------------------------------------
    // 移动端导航栈（波次 2）：栈1 设置首页 → 栈2 分组页（个人设置/组织设置/系统设置）
    // → 栈3 个人设置模块页；桌面端以下逻辑均不触发
    // ------------------------------------------------------------------
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));
    const mobileCanBack = computed(() => canBack(MOBILE_TAB));

    /** 菜单选中：桌面切右栏分组；移动端压入分组页栈帧（栈2 整页） */
    const onSelectMenu = (key: MenuKey) => {
      activeMenu.value = key;
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'section', { key });
      }
    };

    /** 个人设置模块选中：桌面切第四栏；移动端压入模块页栈帧（栈3 整页） */
    const onSelectModule = (key: PersonalModuleKey) => {
      activeModule.value = key;
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'module', { key });
      }
    };

    /** 返回栏：弹出栈顶回上一栏 */
    const onMobileBack = () => popPage(MOBILE_TAB);

    /** 返回栏标题：栈3 为模块名，栈2 为分组名 */
    const mobileTitle = computed(() => {
      const frame = mobileFrame.value;
      if (frame.page === 'module') {
        return personalModules.find((item) => item.key === frame.params?.key)?.label ?? '个人设置';
      }
      return menuItems.value.find((item) => item.key === frame.params?.key)?.label ?? '设置';
    });

    // 栈顶帧变化（重进按栈恢复 / 返回 pop / 复位）时同步选中分组与模块
    watch(
      [mobileFrame, isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        if (frame.page === 'section' && menuItems.value.some((item) => item.key === frame.params?.key)) {
          activeMenu.value = frame.params?.key as MenuKey;
        } else if (frame.page === 'module' && personalModules.some((item) => item.key === frame.params?.key)) {
          activeMenu.value = 'mine';
          activeModule.value = frame.params?.key as PersonalModuleKey;
        }
      },
      { immediate: true }
    );

    const onProfileUpdated = (result: { nickname: string | null; avatar: string | null }) => {
      rootStatus.value = { ...rootStatus.value, nickname: result.nickname, avatar: result.avatar };
      // 通知外壳刷新顶栏身份头像
      emit('profile-updated');
    };

    onMounted(async () => {
      try {
        rootStatus.value = await window.electronAPI.rootIdentity.status();
      } catch {
        // 状态读取失败保留默认展示
      }
      try {
        await refreshOrganizations();
      } catch {
        // 名称读取失败时标题回退「组织空间」
      }
    });

    return {
      activeMenu,
      activeModule,
      personalModules,
      menuItems,
      isPersonal,
      currentOrgName,
      currentSpaceOrgId,
      headerAvatar,
      rootStatus,
      onProfileUpdated,
      isMobileLayout,
      mobileFrame,
      mobileCanBack,
      mobileTitle,
      onSelectMenu,
      onSelectModule,
      onMobileBack
    };
  }
});
</script>

<style src="../styles/pages/mine.css"></style>
<style src="../styles/pages/settings.css"></style>
<!-- 组织子面板（GatewayManager/PublicOrgPanel/RecoverConnectionPanel 等）依赖 org.css 的全局类（原由 OrgPage 引入） -->
<style src="../styles/pages/org.css"></style>
