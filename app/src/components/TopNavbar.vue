<!-- 顶部导航栏（全局固定）：左侧=空间切换器；中间=全局搜索；右侧=网络状态+「⋯」更多菜单（切换账号/退出登录）；
     当前身份头像在 rail 顶部（ui-space-navbar §3/§14） -->
<template>
  <div class="top-navbar">
    <div class="top-navbar-left">
      <SpaceSwitcher />
    </div>

    <div class="top-navbar-center">
      <GlobalSearch />
    </div>

    <div class="top-navbar-right">
      <!-- 网络状态全局常驻（「⋯」前）：个人空间=全局 P2P，组织空间=当前组织副本状态 -->
      <NetworkStatusBar />
      <el-dropdown trigger="click" @command="onMoreCommand">
        <button class="top-navbar-more" title="更多">
          <el-icon :size="18"><MoreFilled /></el-icon>
        </button>
        <template #dropdown>
          <el-dropdown-menu>
            <el-dropdown-item command="switch-account">切换账号</el-dropdown-item>
            <el-dropdown-item command="logout" class="top-navbar-more-danger">退出登录</el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { MoreFilled } from '@element-plus/icons-vue';
import SpaceSwitcher from './SpaceSwitcher.vue';
import NetworkStatusBar from './NetworkStatusBar.vue';
import GlobalSearch from './GlobalSearch.vue';

type MoreCommand = 'switch-account' | 'logout';

export default defineComponent({
  name: 'TopNavbar',
  components: {
    SpaceSwitcher,
    NetworkStatusBar,
    GlobalSearch,
    MoreFilled
  },
  emits: ['switch-account', 'logout'],
  setup(_, { emit }) {
    // 「⋯」菜单命令转发给 App（lock + reload 语义与 RootGate.handleLogout 一致）
    const onMoreCommand = (command: MoreCommand) => {
      emit(command === 'logout' ? 'logout' : 'switch-account');
    };

    return { onMoreCommand };
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

.top-navbar-more {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: 0;
  background: transparent;
  cursor: pointer;
  border-radius: var(--spark-radius-m);
  color: var(--spark-text-2);
  -webkit-app-region: no-drag;
}

.top-navbar-more:hover {
  background: var(--spark-bg-hover);
  color: var(--spark-text-1);
}
</style>

<!-- 下拉菜单挂在 body，scoped 样式够不到，危险色用全局类 -->
<style>
.top-navbar-more-danger {
  color: var(--spark-danger) !important;
}
</style>
