<!-- 我的页（个人设置）四栏架构：rail | 二级导航(280px) | 列表/概览(280px) | 详情/编辑(自适应)。
     MinePage 只做编排（登录门控 / P2P 信息 / 菜单切换），每个模块的第三、四栏为
     components/mine/ 下的独立组件（以 fragment 同时渲染两栏），编辑全部内联在第四栏。
     第三、四栏按 2:3 弹性分配剩余宽度（各最小 280px）；窗口最小宽度 904px（tauri 配置 960px 已覆盖）。
     本页＝系统根级页面（走查修正）：恒为根身份的个人设置，**不随空间切换变化**——
     组织空间的域内身份/成员权限由顶栏「当前身份」菜单的组织身份对话框承载（D2，
     components/topnav/OrgIdentityDialog.vue），移动端在设置 Tab 列表进入（MobileSettingsPage） -->
<template>
  <section class="mine-page">
    <!-- 未登录：登录/注册引导 -->
    <div
      v-if="!rootStatus.initialized || !rootStatus.unlocked"
      class="mine-plain"
    >
      <header class="page-header">
        <div class="page-header-main mine-header-main">
          <div>
            <p class="eyebrow">个人设置</p>
            <h1>我的</h1>
            <p class="lede">
              账号登录前不会显示主界面，先完成 RootID 注册 / 登录。
            </p>
          </div>
        </div>
      </header>
    </div>

    <template v-else>
      <!-- 移动端（波次 2/3）：整页 + 导航栈——栈1 功能菜单，栈2 模块页（返回栏 + 模块），
           栈帧切换经 MobilePageTransition 滑动转场（微信式）。
           M9 起本页不再作独立底栏 Tab，由设置 Tab 的「个人设置」进入（二级页），
           故栈1 顶格带返回栏（回设置列表） -->
      <MobilePageTransition v-if="isMobileLayout" :tab="MOBILE_TAB">
        <!-- 栈1：功能菜单整页（顶部返回栏回设置 Tab 列表页；菜单无头像卡，身份卡在设置列表页） -->
        <div v-if="mobileFrame.page === 'root'" class="mine-menu">
          <MobileBackBar title="个人设置" @back="onBackRoot" />
          <nav class="mine-menu-list">
            <button
              v-for="item in menuItems"
              :key="item.key"
              type="button"
              class="mine-menu-item"
              :class="{ active: activeMenu === item.key }"
              @click="onSelectMenu(item.key)"
            >
              <el-icon
                class="mine-menu-icon"
                :size="17"
                :style="{ color: item.color }"
                ><component :is="item.icon"
              /></el-icon>
              <!-- M1 新设备通知红点：设备管理项有待看通知时挂小红点（进设备管理页即清） -->
              <el-badge
                v-if="item.key === 'devices' && pendingDeviceNotices.length"
                is-dot
              >
                <span class="mine-menu-label">{{ item.label }}</span>
              </el-badge>
              <span v-else class="mine-menu-label">{{ item.label }}</span>
            </button>
          </nav>
        </div>

        <!-- 栈2：模块页（返回栏 + 当前菜单对应模块；移动端详情走抽屉，微信式点字段看详情） -->
        <template v-else-if="mobileFrame.page === 'module'">
          <MobileBackBar :title="activeMenuLabel" @back="onMobileBack" />
          <ProfileModule
            v-if="activeMenu === 'profile'"
            detail-mode="drawer"
            :root-id="rootStatus.rootId ?? ''"
            :nickname="currentUser.nickname"
            :avatar="currentUser.avatar"
            @profile-updated="onProfileUpdated"
          />
          <MyCardModule
            v-else-if="activeMenu === 'card'"
            detail-mode="drawer"
          />
          <BackupModule
            v-else-if="activeMenu === 'backup'"
            detail-mode="drawer"
            :root-id="rootStatus.rootId"
          />
          <DevicesModule
            v-else-if="activeMenu === 'devices'"
            detail-mode="drawer"
            :root-id="rootStatus.rootId ?? ''"
          />
          <!-- 朋友权限（仅聊天+黑名单）；组织空间的成员权限在组织身份对话框（D2） -->
          <PermissionModule
            v-else-if="activeMenu === 'permission'"
            detail-mode="drawer"
            mode="personal"
          />
          <!-- 安全设置 -->
          <SecurityModule
            v-else-if="activeMenu === 'security'"
            detail-mode="drawer"
            :root-id="rootStatus.rootId ?? ''"
          />
          <!-- 存储与副本（M9 并集：原桌面端独有模块并入移动端个人设置） -->
          <StorageModule
            v-else-if="activeMenu === 'storage'"
            @open-backup="onSelectMenu('backup')"
            @open-devices="onSelectMenu('devices')"
          />
        </template>
      </MobilePageTransition>

      <!-- 桌面端（≥769px 渲染逻辑不变）：第二栏功能菜单 + 第三、四栏模块 -->
      <template v-else>
        <!-- 第二栏：用户信息 + 功能菜单 -->
        <div class="mine-menu">
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
              <el-icon
                class="mine-menu-icon"
                :size="17"
                :style="{ color: item.color }"
                ><component :is="item.icon"
              /></el-icon>
              <!-- M1 新设备通知红点：设备管理项有待看通知时挂小红点（进设备管理页即清） -->
              <el-badge
                v-if="item.key === 'devices' && pendingDeviceNotices.length"
                is-dot
              >
                <span class="mine-menu-label">{{ item.label }}</span>
              </el-badge>
              <span v-else class="mine-menu-label">{{ item.label }}</span>
            </button>
          </nav>

          <!-- 退出登录（problem L6）：第一栏最后一项，弱化危险操作样式，与普通条目区分；
               原 rail 身份行「⋯」菜单（含切换账号）已删除 -->
          <div class="mine-menu-footer">
            <button
              type="button"
              class="mine-menu-item mine-menu-logout"
              @click="logout"
            >
              <el-icon class="mine-menu-icon" :size="17"
                ><SwitchButton
              /></el-icon>
              <span class="mine-menu-label">退出登录</span>
            </button>
          </div>
        </div>

        <!-- 第三、四栏：当前菜单对应模块（默认「我的资料」），各模块自带列表栏与详情栏 -->
        <ProfileModule
          v-if="activeMenu === 'profile'"
          :root-id="rootStatus.rootId ?? ''"
          :nickname="currentUser.nickname"
          :avatar="currentUser.avatar"
          @profile-updated="onProfileUpdated"
        />
        <MyCardModule v-else-if="activeMenu === 'card'" />
        <BackupModule
          v-else-if="activeMenu === 'backup'"
          :root-id="rootStatus.rootId"
        />
        <DevicesModule
          v-else-if="activeMenu === 'devices'"
          :root-id="rootStatus.rootId ?? ''"
        />
        <!-- 朋友权限（仅聊天+黑名单）；组织空间的成员权限在组织身份对话框（D2） -->
        <PermissionModule
          v-else-if="activeMenu === 'permission'"
          mode="personal"
        />
        <!-- 安全设置 -->
        <SecurityModule
          v-else-if="activeMenu === 'security'"
          :root-id="rootStatus.rootId ?? ''"
        />
        <!-- 存储与副本（problem L7：原「设置」弹窗个人设置的独有模块，并集合并归入「我的」） -->
        <StorageModule
          v-else-if="activeMenu === 'storage'"
          @open-backup="onSelectMenu('backup')"
          @open-devices="onSelectMenu('devices')"
        />
      </template>
    </template>
  </section>
</template>

<script lang="ts">
import {
  computed,
  defineComponent,
  onMounted,
  ref,
  watch,
  type Component,
} from 'vue';
import { ElMessage } from 'element-plus';
import {
  Coin,
  Key,
  Lock,
  Monitor,
  Postcard,
  SwitchButton,
  Unlock,
  User,
} from '@element-plus/icons-vue';
import { currentUser } from '../stores/current-user';
import { pendingDeviceNotices } from '../stores/device-notices';
import { isMobileLayout } from '../stores/ui-layout';
import {
  currentPage,
  popPage,
  pushPage,
} from '../stores/mobile-nav';
import { lockAndReload } from '../utils/identity-lock';
import type { RootStatusDto as RootStatus } from '../api';
import { personalAvatarSource } from '../stores/avatar-sources';
import UserAvatar from '../components/UserAvatar.vue';
import MobileBackBar from '../components/MobileBackBar.vue';
import MobilePageTransition from '../components/MobilePageTransition.vue';
import ProfileModule from '../components/mine/ProfileModule.vue';
import MyCardModule from '../components/mine/MyCardModule.vue';
import BackupModule from '../components/mine/BackupModule.vue';
import DevicesModule from '../components/mine/DevicesModule.vue';
import PermissionModule from '../components/mine/PermissionModule.vue';
import SecurityModule from '../components/mine/SecurityModule.vue';
import StorageModule from '../components/mine/StorageModule.vue';

type MenuKey =
  | 'profile'
  | 'card'
  | 'backup'
  | 'devices'
  | 'permission'
  | 'security'
  | 'storage';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'mine';

export default defineComponent({
  name: 'MinePage',
  components: {
    UserAvatar,
    MobileBackBar,
    MobilePageTransition,
    ProfileModule,
    MyCardModule,
    BackupModule,
    DevicesModule,
    PermissionModule,
    SecurityModule,
    StorageModule,
    SwitchButton,
    Monitor,
  },
  emits: ['profile-updated', 'back-root'],
  setup(_, { emit }) {
    const rootStatus = ref<RootStatus>({
      initialized: false,
      unlocked: false,
      rootId: null,
      nickname: null,
      avatar: null,
    });
    // 默认选中「我的资料」（根级页面，不随空间变化）
    const activeMenu = ref<MenuKey>('profile');

    // color 为菜单图标色（微信式每项一色，取色与 utils/palette 品牌色板同源，移动端与桌面端统一上色）
    const menuItems = computed<
      Array<{ key: MenuKey; label: string; icon: Component; color: string }>
    >(() => {
      // 「我的」＝根身份的个人设置，是系统根级页面，**不随空间切换变化**（走查修正：
      // rail 展示整个系统根的部分）。组织空间的域内身份/成员权限改由顶栏「当前身份」
      // 菜单的组织身份对话框承载（D2），不再混入本页。
      // 与 SettingsPage personalModules 取并集（problem L7：两处个人设置合并为唯一一份、归「我的」）：
      // 并集 = 我的资料/我的名片/朋友权限/安全设置/账号备份/设备管理 + 存储与副本（原设置侧独有）。
      // M9 起存储与副本同步并入移动端（个人设置整体收进设置 Tab，不再分端删减）。
      // 走查修正：「我的组织」不需要（A2 条目撤下）；账号备份紧随我的名片之后。
      return [
        { key: 'profile', label: '我的资料', icon: User, color: '#3296fa' },
        { key: 'card', label: '我的名片', icon: Postcard, color: '#34c19b' },
        { key: 'backup', label: '账号备份', icon: Key, color: '#7b61ff' },
        { key: 'permission', label: '朋友权限', icon: Lock, color: '#ff7d00' },
        { key: 'security', label: '安全设置', icon: Unlock, color: '#7b61ff' },
        { key: 'devices', label: '设备管理', icon: Monitor, color: '#3296fa' },
        { key: 'storage', label: '存储与副本', icon: Coin, color: '#34c19b' },
      ];
    });

    // 移动端栈帧与菜单项集合不再随空间变化（根级页面口径）；无需空间 watch

    // ------------------------------------------------------------------
    // 移动端导航栈（波次 2）：栈1 功能菜单 → 栈2 模块页；桌面端以下逻辑均不触发
    // ------------------------------------------------------------------
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));

    /** 菜单选中：桌面切右栏模块；移动端压入模块页栈帧（整页） */
    const onSelectMenu = (key: MenuKey) => {
      activeMenu.value = key;
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'module', { key });
      }
    };

    /** 返回栏：弹出栈顶回功能菜单（栈1） */
    const onMobileBack = () => popPage(MOBILE_TAB);

    /** 栈1 返回栏：回来源页（设置 Tab 列表；App.vue backFromSecondaryTab 处理） */
    const onBackRoot = () => emit('back-root');

    // 退出登录（problem L6）：锁定身份后整窗重载回登录/选择账号页，统一走 identity-lock 收敛点
    const logout = () => lockAndReload('已退出登录');

    /** 返回栏标题：当前模块名 */
    const activeMenuLabel = computed(
      () =>
        menuItems.value.find((item) => item.key === activeMenu.value)?.label ??
        '我的',
    );

    // 栈顶帧变化（重进 tab 按栈恢复 / 重按 tab 复位）时同步选中模块
    watch(
      [mobileFrame, isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        const key = frame.params?.key as MenuKey | undefined;
        if (
          frame.page === 'module' &&
          menuItems.value.some((item) => item.key === key)
        ) {
          activeMenu.value = key as MenuKey;
        }
      },
      { immediate: true },
    );

    // 头部身份：恒为根身份（「我的」是系统根级页面，不随空间切换）；
    // 取数统一走 avatar-sources（与 rail 底部「我的」同源）
    const headerSource = computed(() => personalAvatarSource());
    const headerSubtitle = computed(() => '个人设置');

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
    };

    const onProfileUpdated = (result: {
      nickname: string | null;
      avatar: string | null;
    }) => {
      rootStatus.value = {
        ...rootStatus.value,
        nickname: result.nickname,
        avatar: result.avatar,
      };
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
      // 昵称/头像展示统一取 currentUser 单例（SelfProfileSynced 事件会刷新它；
      // 本地 rootStatus 只在挂载/本页保存时更新，跨设备同步到的昵称进不来）
      currentUser,
      activeMenu,
      menuItems,
      pendingDeviceNotices,
      headerSource,
      headerSubtitle,
      onProfileUpdated,
      isMobileLayout,
      mobileFrame,
      MOBILE_TAB,
      activeMenuLabel,
      onSelectMenu,
      onMobileBack,
      onBackRoot,
      logout,
    };
  },
});
</script>

<!-- 非 scoped（同 settings.css）：.mine-list / .mine-detail 等栏位样式由各模块组件共用 -->
<style src="../styles/pages/mine.css"></style>

<style scoped>
/* 退出登录区（problem L6，仅桌面端渲染）：margin-top:auto 置底为菜单栏最后一项，
   弱化危险操作样式（危险色文字 + 危险浅底 hover），与普通条目区分 */
.mine-menu-footer {
  margin-top: auto;
  padding-top: 8px;
  border-top: 1px solid var(--spark-border-light);
}

.mine-menu-logout {
  color: var(--spark-danger);
}

.mine-menu-logout .mine-menu-icon {
  color: var(--spark-danger);
}

.mine-menu-logout:hover {
  background: var(--spark-danger-bg);
}
</style>
