<!-- 顶部上下文条（README §6.1）：当前空间 · 当前身份（域内名）。
     左侧=当前空间名（D1：点击下弹 macOS 风格菜单：切换空间 / 创建空间 / 空间设置——
     个人空间的空间设置＝名字与 logo，PersonalSpaceSettingsDialog）
     · 当前身份（D2：点击下弹菜单，含身份设置——个人空间开「我的·我的资料」，
     组织空间开组织身份对话框 OrgIdentityDialog：域内身份资料＋成员权限）；
     右侧=当前空间网络状态（D3）；当前身份头像在 rail 底部（恒为根身份，L13） -->
<template>
  <div class="top-navbar">
    <div class="top-navbar-left">
      <!-- D1：当前空间点击向下弹菜单（el-dropdown，G3 弹层体系） -->
      <el-dropdown
        trigger="click"
        placement="bottom-start"
        @command="onSpaceCommand"
        @visible-change="onSpaceMenuVisible"
      >
        <span class="context-space context-trigger" :title="`当前空间：${spaceName}`">
          {{ spaceName }}
          <el-icon :size="11" class="context-caret"><ArrowDown /></el-icon>
        </span>
        <template #dropdown>
          <el-dropdown-menu>
            <el-dropdown-item disabled class="topnav-menu-caption">切换空间</el-dropdown-item>
            <el-dropdown-item command="space:personal">
              <el-icon :size="13" class="space-check" :style="{ visibility: isPersonal ? 'visible' : 'hidden' }">
                <Check />
              </el-icon>
              {{ personalSpaceName }}
            </el-dropdown-item>
            <el-dropdown-item
              v-for="org in organizations"
              :key="org.orgId"
              :command="`space:${org.orgId}`"
            >
              <el-icon
                :size="13"
                class="space-check"
                :style="{ visibility: currentSpaceOrgId === org.orgId ? 'visible' : 'hidden' }"
              >
                <Check />
              </el-icon>
              {{ org.name }}
            </el-dropdown-item>
            <el-dropdown-item divided command="create">创建空间</el-dropdown-item>
            <!-- 空间设置：组织空间=OrgSettingsPanel；个人空间=名字与 logo（本机偏好） -->
            <el-dropdown-item command="settings">空间设置</el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>

      <!-- D2：当前身份点击向下弹菜单，内含身份设置（随空间不同而不同） -->
      <el-dropdown trigger="click" placement="bottom-start" @command="onIdentityCommand">
        <span class="context-identity context-trigger" :title="`当前身份：${identityName}`">
          <el-icon :size="13"><User /></el-icon>
          {{ identityName }}
          <el-icon :size="11" class="context-caret"><ArrowDown /></el-icon>
        </span>
        <template #dropdown>
          <el-dropdown-menu>
            <el-dropdown-item disabled class="topnav-menu-caption">{{ identityScopeText }}</el-dropdown-item>
            <el-dropdown-item command="identity-settings">身份设置</el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>
    </div>

    <div class="top-navbar-right">
      <!-- D3：当前空间维度网络状态（组织空间=当前组织副本状态，个人空间=个人空间网络），
           与左栏「我的」里的全局/设备维度状态区分；弹层文案已标明口径。
           「⋯」菜单已移至 rail 底部头像右侧（用户评审决策） -->
      <NetworkStatusBar />
    </div>

    <!-- 空间设置（D1）：组织空间=OrgSettingsPanel；个人空间=名字与 logo（本机偏好） -->
    <SpaceSettingsDialog v-model="spaceSettingsVisible" />
    <PersonalSpaceSettingsDialog v-model="personalSpaceSettingsVisible" />
    <!-- 身份设置（D2）：组织空间=域内身份＋成员权限对话框；个人空间走「我的」（MinePage） -->
    <OrgIdentityDialog v-model="orgIdentityVisible" />
  </div>
</template>

<script lang="ts">
import { defineComponent, computed, ref } from 'vue';
import { ArrowDown, Check, User } from '@element-plus/icons-vue';
import NetworkStatusBar from './NetworkStatusBar.vue';
import SpaceSettingsDialog from './topnav/SpaceSettingsDialog.vue';
import PersonalSpaceSettingsDialog from './topnav/PersonalSpaceSettingsDialog.vue';
import OrgIdentityDialog from './topnav/OrgIdentityDialog.vue';
import { isMobileLayout } from '../stores/ui-layout';
import { currentSpace, currentSpaceOrgId, switchToOrg, switchToPersonal } from '../stores/current-space';
import { currentUser } from '../stores/current-user';
import { getOrgIdentity } from '../stores/org-identity';
import { findOrg, organizations, refreshOrganizations } from '../stores/org-membership';
import { openMembershipDialog } from '../stores/org-membership-dialog';
import { openShellModal } from '../stores/shell-modal';
import { personalSpaceName } from '../stores/personal-space';

export default defineComponent({
  name: 'TopNavbar',
  components: {
    NetworkStatusBar,
    SpaceSettingsDialog,
    PersonalSpaceSettingsDialog,
    OrgIdentityDialog,
    ArrowDown,
    Check,
    User
  },
  setup() {
    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    const spaceSettingsVisible = ref(false);
    const personalSpaceSettingsVisible = ref(false);
    const orgIdentityVisible = ref(false);

    // 域内身份昵称（上下文条）：组织空间显该域昵称（缺省回退根身份），个人空间显根身份
    const identityName = computed(() => {
      if (currentSpace.value.type === 'org') {
        const orgName = getOrgIdentity(currentSpace.value.orgId).nickname;
        return orgName || currentUser.nickname || '未命名';
      }
      return currentUser.nickname || '未命名';
    });

    // 当前空间名（上下文条主体）：个人空间（可自定义名，stores/personal-space）/ 组织名
    const spaceName = computed(() =>
      currentSpace.value.type === 'personal'
        ? personalSpaceName.value
        : findOrg(currentSpaceOrgId.value)?.name ?? '组织空间'
    );

    // D2 身份菜单的口径说明行：身份设置随空间不同（个人=我的资料；组织=该域内身份资料）
    const identityScopeText = computed(() =>
      isPersonal.value ? '个人空间身份' : `${spaceName.value} 的域内身份`
    );

    // 打开空间菜单时刷新一次组织列表，保证「切换空间」列出最新已加入组织
    const onSpaceMenuVisible = (visible: boolean) => {
      if (visible) {
        void refreshOrganizations().catch(() => {
          // 读取失败保留缓存列表
        });
      }
    };

    const onSpaceCommand = (command: string) => {
      if (command === 'create') {
        // 复用 L10 顶级对话框：创建组织
        openMembershipDialog('create');
        return;
      }
      if (command === 'settings') {
        if (isPersonal.value) {
          personalSpaceSettingsVisible.value = true;
        } else {
          spaceSettingsVisible.value = true;
        }
        return;
      }
      if (command.startsWith('space:')) {
        const target = command.slice('space:'.length);
        if (target === 'personal') {
          switchToPersonal();
        } else {
          switchToOrg(target);
        }
      }
    };

    const onIdentityCommand = (command: string) => {
      if (command !== 'identity-settings') {
        return;
      }
      // 身份设置随空间不同（D2）：个人空间走 L12 壳层「我的」（MinePage，根身份个人设置）；
      // 组织空间开组织身份对话框（域内身份资料＋成员权限，OrgIdentityDialog）——
      // 「我的」是系统根级页面不随空间变化，域内身份不再混入其中（走查修正）
      if (isPersonal.value) {
        openShellModal('mine');
      } else {
        orgIdentityVisible.value = true;
      }
    };

    return {
      isMobileLayout,
      isPersonal,
      identityName,
      identityScopeText,
      spaceName,
      organizations,
      currentSpaceOrgId,
      spaceSettingsVisible,
      personalSpaceSettingsVisible,
      orgIdentityVisible,
      personalSpaceName,
      onSpaceMenuVisible,
      onSpaceCommand,
      onIdentityCommand
    };
  }
});
</script>

<style scoped>
.top-navbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
}

.top-navbar-left {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
  flex: 1;
}

/* 当前空间名（上下文条主体）；D1 起为可点击触发器 */
.context-space {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 200px;
  flex-shrink: 1;
  padding: 3px 6px;
  border-radius: var(--spark-radius-m);
  cursor: pointer;
  -webkit-app-region: no-drag;
}

.context-space:hover {
  background: var(--spark-bg-hover);
}

/* 域内身份昵称（上下文条 §6.1）：弱态展示，不抢空间名主体；D2 起为可点击触发器 */
.context-identity {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-left: 6px;
  padding: 3px 8px;
  border-radius: var(--spark-radius-m);
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-2);
  background: var(--spark-bg-hover);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 180px;
  flex-shrink: 1;
  cursor: pointer;
  -webkit-app-region: no-drag;
}

.context-identity:hover {
  color: var(--spark-text-1);
}

.context-caret {
  flex-shrink: 0;
  opacity: 0.6;
}

/* 中间全局搜索：flex 伸展至顶栏约 1/3 宽度，min/max 防止极端窗口变形 */
.top-navbar-center {
  flex: 1 1 0;
  min-width: 220px;
  max-width: 480px;
  display: flex;
  justify-content: center;
}

.top-navbar-right {
  display: inline-flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  flex: 1;
  flex-shrink: 0;
  min-width: 0;
}

/* 窄屏（≤768px，与 stores/ui-layout.ts 同一断点）：顶栏内容紧凑化，
   允许全局搜索收缩、右侧区间距收窄；桌面端不受影响 */
@media (max-width: 768px) {
  .top-navbar-center {
    min-width: 0;
  }

  .top-navbar-right {
    gap: 4px;
  }
}
</style>

<!-- 下拉菜单挂在 body，scoped 样式够不到：勾选位 / 说明行 / 危险色用全局类 -->
<style>
.topnav-menu-caption {
  font-size: var(--spark-font-size-secondary) !important;
  color: var(--spark-text-3) !important;
  cursor: default !important;
}

/* 切换空间项的勾选位：无勾也占位，保持文字对齐（macOS 菜单勾选列） */
.space-check {
  margin-right: 4px;
}

.top-navbar-more-danger {
  color: var(--spark-danger) !important;
}
</style>
