<!-- 移动端顶部导航（Android 前端改造）：
     左侧=菜单图标（打开左滑侧边栏 MobileSpaceDrawer）；中间=当前页名（消息/通讯录/应用/我的）+
     页名右侧紧贴网络状态点（点击直达系统设置→网络状态）；
     右侧=搜索（全屏搜索层，复用 GlobalSearch）+ 圆圈加号（下拉菜单，微信式：
     个人空间「添加朋友」/ 组织空间「添加成员」（仅组织管理员），经 App.vue 接到通讯录现有添加流程）。
     仅在四个主 tab 且栈深=1（currentPage(tab).page==='root'）时由 App.vue 渲染，
     进入二级页（聊天/详情等）时顶部导航整体不渲染（App.vue）。 -->
<template>
  <header class="mobile-top-bar">
    <button type="button" class="mobile-top-bar-btn" title="切换空间" @click="emit('open-drawer')">
      <!-- 抽屉图标：内联 SVG 自绘「上长下短」两根横线（企业微信风格，1.6px 粗、圆角线帽；
           两端各留 1px 避免圆角线帽被视口裁切），不用 element-plus 菜单图标 -->
      <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
        <path d="M1 5.9h18" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        <path d="M1 14.1h12" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
      </svg>
    </button>

    <!-- 中间：页名 + 网络状态点（点紧贴文字右侧，随文字长度自适应） -->
    <div class="mobile-top-bar-title">
      <span class="mobile-top-bar-title-text">{{ title }}</span>
      <NetworkStatusBar variant="dot" @open-network-status="emit('open-network-status')" />
    </div>

    <!-- 右侧：搜索 + 加号（仅一级页顶栏） -->
    <div class="mobile-top-bar-actions">
      <button type="button" class="mobile-top-bar-btn" title="搜索" @click="openSearch">
        <el-icon :size="20"><Search /></el-icon>
      </button>
      <button type="button" class="mobile-top-bar-btn" title="添加" @click="addSheetVisible = true">
        <el-icon :size="20"><CirclePlus /></el-icon>
      </button>
    </div>
  </header>

  <!-- 全屏搜索层：复用 GlobalSearch 的搜索与跳转能力（其下拉即结果区），选中结果/取消后关闭 -->
  <Teleport to="body">
    <div v-if="searchVisible" class="mobile-search-layer">
      <div class="mobile-search-bar">
        <GlobalSearch ref="searchRef" @select="searchVisible = false" />
        <button type="button" class="mobile-search-cancel" @click="searchVisible = false">取消</button>
      </div>
    </div>
  </Teleport>

  <!-- 加号下拉菜单（微信式：从右上角加号按钮向下展开的卡片，小三角指向加号）：
       按空间区分入口；点击遮罩/外部收起（下拉卡片无「取消」按钮） -->
  <Teleport to="body">
    <Transition name="mobile-add-sheet">
      <div v-if="addSheetVisible" class="mobile-add-sheet-root" @click="addSheetVisible = false">
        <div class="mobile-add-sheet" @click.stop>
          <!-- 「添加朋友」仅个人空间；「添加成员」仅组织空间且当前用户是管理员
               （与通讯录页内「添加成员」按钮的 isOrgAdmin 门控同口径，非管理员不显示） -->
          <button v-if="isPersonal" type="button" class="mobile-add-sheet-item" @click="pickAddFriend">
            <el-icon :size="18" :style="{ color: '#ff7d00' }"><User /></el-icon>
            <div class="mobile-add-sheet-item-text">
              <b>添加朋友</b>
              <span>通过身份 ID 添加新的朋友</span>
            </div>
          </button>
          <button v-else-if="isOrgAdmin" type="button" class="mobile-add-sheet-item" @click="pickAddMember">
            <el-icon :size="18" :style="{ color: '#3296fa' }"><Avatar /></el-icon>
            <div class="mobile-add-sheet-item-text">
              <b>添加成员</b>
              <span>生成邀请码，邀请加入当前组织</span>
            </div>
          </button>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, ref } from 'vue';
import { Avatar, CirclePlus, Search, User } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId } from '../stores/current-space';
import { isAdmin } from '../stores/org-membership';
import NetworkStatusBar from './NetworkStatusBar.vue';
import GlobalSearch from './GlobalSearch.vue';

export default defineComponent({
  name: 'MobileTopBar',
  components: { Search, CirclePlus, User, Avatar, NetworkStatusBar, GlobalSearch },
  props: {
    /** 当前页名（消息/通讯录/应用/我的） */
    title: { type: String, required: true }
  },
  emits: ['open-drawer', 'open-network-status', 'add-friend', 'add-member'],
  setup(_, { emit }) {
    // 全屏搜索层
    const searchVisible = ref(false);
    const searchRef = ref<{ focusInput?: () => void } | null>(null);
    const openSearch = async () => {
      searchVisible.value = true;
      // 等搜索层渲染完成后聚焦输入框（唤起键盘）
      await nextTick();
      searchRef.value?.focusInput?.();
    };

    // 加号下拉菜单：菜单项按当前空间区分（个人=添加朋友；组织=添加成员）
    const addSheetVisible = ref(false);
    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    // 「添加成员」仅组织管理员可见（org-membership 缓存口径，同 use-contacts-data 的 isOrgAdmin；
    // 缓存由 App.vue 启动校验与各页 refresh 填充，未加载时按非管理员处理）
    const isOrgAdmin = computed(() => isAdmin(currentSpaceOrgId.value));
    const pickAddFriend = () => {
      addSheetVisible.value = false;
      emit('add-friend');
    };
    const pickAddMember = () => {
      addSheetVisible.value = false;
      emit('add-member');
    };

    return {
      emit,
      searchVisible,
      searchRef,
      openSearch,
      addSheetVisible,
      isPersonal,
      isOrgAdmin,
      pickAddFriend,
      pickAddMember
    };
  }
});
</script>

<style scoped>
.mobile-top-bar {
  flex-shrink: 0;
  display: grid;
  grid-template-columns: 1fr auto 1fr;
  align-items: center;
  width: 100%;
  height: 100%;
}

.mobile-top-bar-btn {
  justify-self: start;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  margin: 0;
  padding: 0;
  border: 0;
  background: transparent;
  border-radius: var(--spark-radius-m);
  color: var(--spark-text-1);
  cursor: pointer;
  font-family: inherit;
  -webkit-app-region: no-drag;
}

.mobile-top-bar-btn:hover {
  background: var(--spark-bg-hover);
}

/* 中间：页名 + 网络点同一行，点紧贴文字右侧（gap 小、不固定宽度） */
.mobile-top-bar-title {
  justify-self: center;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  max-width: 100%;
}

.mobile-top-bar-title-text {
  font-size: 16px;
  font-weight: 600;
  color: var(--spark-text-1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 右侧操作组：搜索 + 加号两枚图标（微信式顶栏右侧） */
.mobile-top-bar-actions {
  justify-self: end;
  display: inline-flex;
  align-items: center;
  gap: 2px;
}

.mobile-top-bar-actions .mobile-top-bar-btn {
  justify-self: auto;
}

/* ---- 全屏搜索层 ---- */
.mobile-search-layer {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: var(--spark-bg-card);
  /* 避开系统状态栏（safe-area-inset-top 桌面端恒为 0；本组件桌面端本就不渲染） */
  padding-top: env(safe-area-inset-top, 0px);
}

.mobile-search-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--spark-border-light);
}

.mobile-search-bar :deep(.global-search) {
  flex: 1;
  min-width: 0;
  width: auto;
}

/* 结果区铺满搜索层剩余高度（桌面下拉 420px 上限在移动端全屏层放开）；
   min-width:0 覆盖 GlobalSearch 下拉的 320px 兜底宽度，避免窄屏（<320px+边距）溢出 */
.mobile-search-bar :deep(.global-search-dropdown) {
  max-height: calc(100dvh - 64px - env(safe-area-inset-top, 0px));
  min-width: 0;
}

.mobile-search-cancel {
  flex-shrink: 0;
  margin: 0;
  padding: 6px 4px;
  border: 0;
  background: transparent;
  font-family: inherit;
  font-size: 15px;
  color: var(--spark-text-2);
  cursor: pointer;
}

/* ---- 加号下拉菜单（第三批打磨，微信式）：从右上角加号按钮向下展开的卡片，小三角指向加号 ---- */
/* 遮罩透明：仅承接「点外部收起」，不压暗背景 */
.mobile-add-sheet-root {
  position: fixed;
  inset: 0;
  z-index: 2010;
}

/* 下拉卡片：顶栏（44px + 状态栏安全区）之下、右对齐顶栏右缘（顶栏右 padding 8px） */
.mobile-add-sheet {
  position: absolute;
  top: calc(var(--spark-topbar-height) + env(safe-area-inset-top, 0px) + 4px);
  right: 8px;
  width: 250px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  box-shadow: var(--spark-shadow-pop);
  padding: 6px;
  display: flex;
  flex-direction: column;
}

/* 小三角：指向加号按钮中心（顶栏右 padding 8px + 按钮半宽 18px = 距视口右缘 26px，
   再减卡片右偏 8px 与三角半宽 6px → 距卡片右缘 12px） */
.mobile-add-sheet::before {
  content: '';
  position: absolute;
  top: -5px;
  right: 12px;
  width: 12px;
  height: 12px;
  background: var(--spark-bg-card);
  border-top: 1px solid var(--spark-border-light);
  border-left: 1px solid var(--spark-border-light);
  border-top-left-radius: 3px;
  transform: rotate(45deg);
}

.mobile-add-sheet-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  padding: 12px 10px;
  border-radius: var(--spark-radius-m);
  text-align: left;
  color: var(--spark-text-1);
}

.mobile-add-sheet-item:hover {
  background: var(--spark-bg-hover);
}

.mobile-add-sheet-item-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mobile-add-sheet-item-text b {
  font-size: 16px;
  font-weight: 500;
}

.mobile-add-sheet-item-text span {
  font-size: 13px;
  color: var(--spark-text-3);
}

/* 下拉展开过渡：遮罩淡入淡出，卡片从右上角（加号处）缩放+下沉展开 */
.mobile-add-sheet-enter-active,
.mobile-add-sheet-leave-active {
  transition: opacity 180ms ease;
}

.mobile-add-sheet-enter-active .mobile-add-sheet,
.mobile-add-sheet-leave-active .mobile-add-sheet {
  transition:
    transform 200ms cubic-bezier(0.25, 0.46, 0.45, 0.94),
    opacity 200ms ease;
  transform-origin: top right;
}

.mobile-add-sheet-enter-from,
.mobile-add-sheet-leave-to {
  opacity: 0;
}

.mobile-add-sheet-enter-from .mobile-add-sheet,
.mobile-add-sheet-leave-to .mobile-add-sheet {
  transform: scale(0.9) translateY(-6px);
}
</style>
