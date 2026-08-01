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
            <!-- 窄屏（移动端布局）：rail 不渲染，设置/测试入口挪到此处（ui-space-navbar §6.4 形态沿用，
                 测试入口可见性逻辑与 rail 一致——当前均常驻显示，发版隐藏另行处理） -->
            <el-dropdown-item v-if="isMobileLayout" command="settings">设置</el-dropdown-item>
            <el-dropdown-item v-if="isMobileLayout" command="test">测试</el-dropdown-item>
            <el-dropdown-item :divided="isMobileLayout" command="switch-account">切换账号</el-dropdown-item>
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
import { isMobileLayout } from '../stores/ui-layout';

type MoreCommand = 'switch-account' | 'logout' | 'settings' | 'test';

export default defineComponent({
  name: 'TopNavbar',
  components: {
    SpaceSwitcher,
    NetworkStatusBar,
    GlobalSearch,
    MoreFilled
  },
  emits: ['switch-account', 'logout', 'open-tab'],
  setup(_, { emit }) {
    // 「⋯」菜单命令转发给 App：设置/测试（窄屏专属）走 open-tab 切 tab，
    // 切换账号/退出登录维持 lock + reload 语义（与 RootGate.handleLogout 一致）
    const onMoreCommand = (command: MoreCommand) => {
      if (command === 'settings' || command === 'test') {
        emit('open-tab', command);
        return;
      }
      emit(command === 'logout' ? 'logout' : 'switch-account');
    };

    return { onMoreCommand, isMobileLayout };
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

<!-- 下拉菜单挂在 body，scoped 样式够不到，危险色用全局类 -->
<style>
.top-navbar-more-danger {
  color: var(--spark-danger) !important;
}
</style>
