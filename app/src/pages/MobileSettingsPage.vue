<!-- 移动端底栏第 5 Tab「设置」（M3/M9，docs/ui/problem.md）：设置列表页——
     顶部身份卡（头像 + 根身份昵称，可点进个人设置，M38 口径顺带满足），
     下设四项：个人设置 ｜ 系统设置 ｜ 测试 ｜ 退出登录（弱化危险样式、置底）。
     本页只做导航壳、不复制实现：
       个人设置 = 现有 MinePage 整页（切 'mine' 二级页，原「我的」内容全部由其承接）；
       系统设置 = SystemSettingsPanel（原 SettingsPage 的系统设置部分，压入本页栈2 整页）；
       测试     = TestPage（切 'test' 二级页）。
     仅移动端挂载（App.vue 移动端 activeTab==='settings' 分支）；桌面端「系统设置」仍走
     ShellModals + SettingsPage（L5/L12），不受影响。 -->
<template>
  <section class="mine-page mobile-settings-page">
    <MobilePageTransition :tab="MOBILE_TAB">
      <!-- 栈1：身份卡 + 设置菜单整页 -->
      <div v-if="mobileFrame.page === 'root'" class="mine-menu mobile-settings-root">
        <!-- 身份卡：头像 + 根身份昵称，点击进个人设置（我的资料） -->
        <button type="button" class="mobile-settings-identity" @click="openPersonal">
          <UserAvatar
            :root-id="identitySource.seed"
            :nickname="identitySource.name"
            :avatar="identitySource.image"
            :size="44"
          />
          <span class="mobile-settings-identity-name">{{ identitySource.name }}</span>
          <el-icon class="mobile-settings-identity-arrow"><ArrowRight /></el-icon>
        </button>

        <nav class="mine-menu-list">
          <button type="button" class="mine-menu-item" @click="openPersonal">
            <el-icon class="mine-menu-icon" :size="17" :style="{ color: '#3296fa' }"><User /></el-icon>
            <span class="mine-menu-label">个人设置</span>
          </button>
          <!-- 组织空间下的域内身份 / 成员权限（D2 移动端落点；「我的」已改为根级页面不随空间变，
               故组织相关入口挂在这里、仅组织空间显示） -->
          <button v-if="isOrgSpace" type="button" class="mine-menu-item" @click="openOrgIdentity">
            <el-icon class="mine-menu-icon" :size="17" :style="{ color: '#00b8a9' }"><OfficeBuilding /></el-icon>
            <span class="mine-menu-label">组织身份</span>
          </button>
          <button v-if="isOrgSpace" type="button" class="mine-menu-item" @click="openOrgPermission">
            <el-icon class="mine-menu-icon" :size="17" :style="{ color: '#ff7d00' }"><Lock /></el-icon>
            <span class="mine-menu-label">成员权限</span>
          </button>
          <button type="button" class="mine-menu-item" @click="openSystem">
            <el-icon class="mine-menu-icon" :size="17" :style="{ color: '#64748b' }"><Setting /></el-icon>
            <span class="mine-menu-label">系统设置</span>
          </button>
          <button type="button" class="mine-menu-item" @click="openTest">
            <el-icon class="mine-menu-icon" :size="17" :style="{ color: '#94a3b8' }"><Cpu /></el-icon>
            <span class="mine-menu-label">测试</span>
          </button>
        </nav>

        <!-- 退出登录：置底 + 弱化危险样式（同 SettingsPage 账号操作区口径） -->
        <div class="mine-menu-account mobile-settings-logout">
          <button type="button" class="mine-menu-item mine-menu-danger" @click="logout">
            <el-icon class="mine-menu-icon" :size="17"><CircleCloseFilled /></el-icon>
            <span class="mine-menu-label">退出登录</span>
          </button>
        </div>
      </div>

      <!-- 栈2：系统设置（SystemSettingsPanel 自带 section 整页覆盖与内层返回栏；
           覆盖层以本页 section（.mine-page，position:relative）为定位基准，顶部让出返回栏高度） -->
      <template v-else-if="mobileFrame.page === 'system'">
        <MobileBackBar title="系统设置" @back="onMobileBack" />
        <SystemSettingsPanel :initial-section="systemInitialSection ?? undefined" />
      </template>

      <!-- 栈2：组织身份 / 成员权限（仅组织空间；模块走抽屉详情，同 MinePage 移动端口径） -->
      <template v-else-if="mobileFrame.page === 'org-identity'">
        <MobileBackBar title="组织身份" @back="onMobileBack" />
        <OrgIdentityModule detail-mode="drawer" />
      </template>
      <template v-else-if="mobileFrame.page === 'org-permission'">
        <MobileBackBar title="成员权限" @back="onMobileBack" />
        <PermissionModule mode="org" detail-mode="drawer" />
      </template>
    </MobilePageTransition>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ArrowRight, CircleCloseFilled, Cpu, Lock, OfficeBuilding, Setting, User } from '@element-plus/icons-vue';
import { currentPage, popPage, pushPage } from '../stores/mobile-nav';
import { consumePendingSystemSection, type SystemSectionKey } from '../stores/pending-system-section';
import { personalAvatarSource } from '../stores/avatar-sources';
import { currentSpace } from '../stores/current-space';
import { lockAndReload } from '../utils/identity-lock';
import UserAvatar from '../components/UserAvatar.vue';
import MobileBackBar from '../components/MobileBackBar.vue';
import MobilePageTransition from '../components/MobilePageTransition.vue';
import SystemSettingsPanel from '../components/settings/SystemSettingsPanel.vue';
import OrgIdentityModule from '../components/mine/OrgIdentityModule.vue';
import PermissionModule from '../components/mine/PermissionModule.vue';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致；设置为五个主 tab 之一） */
const MOBILE_TAB = 'settings';

export default defineComponent({
  name: 'MobileSettingsPage',
  components: {
    UserAvatar,
    MobileBackBar,
    MobilePageTransition,
    SystemSettingsPanel,
    OrgIdentityModule,
    PermissionModule,
    ArrowRight,
    CircleCloseFilled,
    Cpu,
    Lock,
    OfficeBuilding,
    Setting,
    User
  },
  emits: ['open-tab'],
  setup(_, { emit }) {
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));
    // 系统设置深链初始 section（顶栏网络状态点直达 netStatus；消费后保持，仅首次挂载生效）
    const systemInitialSection = ref<SystemSectionKey | null>(null);

    // 身份卡取数：个人空间根身份（stores/avatar-sources，与 rail/空间切换器同源）
    const identitySource = computed(() => personalAvatarSource());
    /** 组织空间下显示 组织身份 / 成员权限 入口（D2 移动端落点） */
    const isOrgSpace = computed(() => currentSpace.value.type === 'org');

    /** 个人设置：切到 MinePage（'mine' 二级页，App.vue 记录来源 tab 为本页） */
    const openPersonal = () => emit('open-tab', 'mine');
    /** 系统设置：压入本页栈2 整页（面板内部 section 再整页覆盖） */
    const openSystem = () => pushPage(MOBILE_TAB, 'system');
    /** 组织身份 / 成员权限：压入本页栈2（仅组织空间显示入口） */
    const openOrgIdentity = () => pushPage(MOBILE_TAB, 'org-identity');
    const openOrgPermission = () => pushPage(MOBILE_TAB, 'org-permission');
    /** 测试：切到 TestPage（'test' 二级页，同原 SettingsPage goTest 口径） */
    const openTest = () => emit('open-tab', 'test');

    /** 栈2 返回栏：弹栈回设置列表（栈1） */
    const onMobileBack = () => popPage(MOBILE_TAB);

    const logout = () => lockAndReload('已退出登录');

    onMounted(() => {
      // 网络状态点深链：消费请求 → 压入系统设置栈帧 → 子面板定位指定 section
      const pendingSection = consumePendingSystemSection();
      if (pendingSection) {
        systemInitialSection.value = pendingSection;
        pushPage(MOBILE_TAB, 'system');
      }
    });

    return {
      MOBILE_TAB,
      mobileFrame,
      systemInitialSection,
      identitySource,
      isOrgSpace,
      openPersonal,
      openSystem,
      openOrgIdentity,
      openOrgPermission,
      openTest,
      onMobileBack,
      logout
    };
  }
});
</script>

<!-- 非 scoped：.mine-menu / .mine-menu-item 等栏位类与 MinePage/SettingsPage 共用（mine.css） -->
<style src="../styles/pages/mine.css"></style>

<style scoped>
/* 栈1 整页：纵向 flex，退出登录区置底 */
.mobile-settings-root {
  display: flex;
  flex-direction: column;
}

/* 身份卡：与菜单项同一行高语言（58px+），底部与菜单留分隔 */
.mobile-settings-identity {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  margin: 0;
  padding: 14px 16px;
  border: 0;
  border-bottom: 1px solid var(--spark-border-light);
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  text-align: left;
}

.mobile-settings-identity:hover {
  background: var(--spark-bg-hover);
}

.mobile-settings-identity-name {
  flex: 1;
  min-width: 0;
  font-size: 16px;
  font-weight: 600;
  color: var(--spark-text-1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mobile-settings-identity-arrow {
  flex-shrink: 0;
  color: var(--spark-text-3);
}

/* 退出登录置底（栈1 整页 flex 下压到底部） */
.mobile-settings-logout {
  margin-top: auto;
}
</style>
