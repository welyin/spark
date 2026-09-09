<!-- 顶部上下文条（README §6.1）：当前身份（域内名）· 空间名称 · 同步状态。
     左侧=当前身份（域内名）· 当前空间名；中间=全局搜索；右侧=网络状态+「⋯」菜单；
     当前身份头像在 rail 顶部（ui-space-navbar §3/§14） -->
<template>
  <div class="top-navbar">
    <div class="top-navbar-left">
      <!-- 当前空间名 + 域内身份昵称（上下文条 §6.1）；空间切换已移至 rail「空间」二级列表 -->
      <span class="context-space" :title="`当前空间：${spaceName}`">{{ spaceName }}</span>
      <span class="context-identity" :title="`当前身份：${identityName}`">
        <el-icon :size="13"><User /></el-icon>
        {{ identityName }}
      </span>
    </div>

    <div class="top-navbar-right">
      <!-- 网络状态全局常驻：个人空间=全局 P2P，组织空间=当前组织副本状态。
           「⋯」菜单已移至 rail 底部头像右侧（用户评审决策） -->
      <NetworkStatusBar />
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent, computed } from 'vue';
import { User } from '@element-plus/icons-vue';
import NetworkStatusBar from './NetworkStatusBar.vue';
import { isMobileLayout } from '../stores/ui-layout';
import { currentSpace, currentSpaceOrgId } from '../stores/current-space';
import { currentUser } from '../stores/current-user';
import { getOrgIdentity } from '../stores/org-identity';
import { findOrg } from '../stores/org-membership';

export default defineComponent({
  name: 'TopNavbar',
  components: {
    NetworkStatusBar,
    User
  },
  setup() {
    // 域内身份昵称（上下文条）：组织空间显该域昵称（缺省回退根身份），个人空间显根身份
    const identityName = computed(() => {
      if (currentSpace.value.type === 'org') {
        const orgName = getOrgIdentity(currentSpace.value.orgId).nickname;
        return orgName || currentUser.nickname || '未命名';
      }
      return currentUser.nickname || '未命名';
    });

    // 当前空间名（上下文条主体）：个人空间 / 组织名
    const spaceName = computed(() =>
      currentSpace.value.type === 'personal'
        ? '个人空间'
        : findOrg(currentSpaceOrgId.value)?.name ?? '组织空间'
    );

    return { isMobileLayout, identityName, spaceName };
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

/* 当前空间名（上下文条主体，替代原 SpaceSwitcher 触发器位） */
.context-space {
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 200px;
  flex-shrink: 1;
}

/* 域内身份昵称（上下文条 §6.1）：弱态展示，不抢空间名主体 */
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
