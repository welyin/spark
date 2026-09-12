<!-- 事务页（阶段 3 / README §4.4、shell-desktop §2.4）：跨全部域「与我相关」的事务列表。
     「等我操作」（进行中）置顶高亮分组，已关闭在后；只做与我相关，公共议题是空间插件（README §八决策 2）。
     点卡片按事务类型打开对应类型插件视图（3.4 deep-link 承接；当前版本先跳转到事务详情插件）。
     双端：PC 三栏（筛选 | 列表 | 插件视图）为终态，第一版先落地列表 + 点击分发；
     移动端列表 + 去处理大按钮（shell-mobile §四）。
     M7/M8 走查修正：页面自带头部（标题+计数+刷新按钮）整条移除——计数并入列表区顶部 chip，
     刷新改微博式下拉手势（PullRefresh 容器）。 -->
<template>
  <section ref="rootEl" class="affairs-page">
    <PullRefresh :on-refresh="refresh">
      <div class="affairs-body">
        <!-- 待处理计数 chip（M7：原头部计数并入列表区顶部） -->
        <div v-if="actionableCount > 0" class="affairs-chip-row">
          <span class="affairs-count">待我处理 {{ actionableCount }}</span>
        </div>

        <!-- A6：可组合筛选收成顶部 chips（同维度内 OR、跨维度 AND；「等我操作」分组不受筛选影响、永远置顶） -->
        <div v-if="hasAnyFilterSource" class="affairs-filters">
          <div class="affairs-filter-row">
            <button
              v-for="opt in STATUS_OPTIONS"
              :key="opt.key"
              type="button"
              class="affair-filter-chip"
              :class="{ active: filters.status.includes(opt.key) }"
              @click="toggleStatus(opt.key)"
            >
              {{ opt.label }}
            </button>
            <button
              v-for="opt in RELATION_OPTIONS"
              :key="opt.key"
              type="button"
              class="affair-filter-chip"
              :class="{ active: filters.relation.includes(opt.key) }"
              @click="toggleRelation(opt.key)"
            >
              {{ opt.label }}
            </button>
            <button
              v-for="tag in allTags"
              :key="tag"
              type="button"
              class="affair-filter-chip"
              :class="{ active: filters.tags.includes(tag) }"
              @click="toggleTag(tag)"
            >
              # {{ tag }}
            </button>
            <button
              v-if="hasActiveFilter"
              type="button"
              class="affair-filter-chip affair-filter-clear"
              @click="clearFilters"
            >
              清除筛选 ×
            </button>
          </div>
        </div>

        <div v-if="error" class="affairs-error">{{ error }}</div>

        <template v-else>
          <!-- 等我操作（进行中）置顶高亮分组 -->
          <div v-if="grouped.open.length > 0" class="affair-group">
            <div class="affair-group-title affair-group-title--action">
              等我操作（{{ grouped.open.length }}）
            </div>
            <button
              v-for="item in grouped.open"
              :key="item.affairId"
              type="button"
              class="affair-card affair-card--open"
              @click="openAffair(item)"
              @touchstart="lp.start($event, item)"
              @touchmove="lp.move"
              @touchend="lp.end"
              @touchcancel="lp.end"
              @contextmenu.prevent="onCardMenu($event, item)"
            >
              <div class="affair-card-main">
                <span class="affair-card-title"
                  ><el-icon v-if="isAffairFavorited(item.affairId)" class="affair-card-fav" :size="13"><StarFilled /></el-icon
                  >{{ item.title }}</span
                >
                <span v-if="item.summary" class="affair-card-summary">{{
                  item.summary
                }}</span>
                <div class="affair-card-meta">
                  <span
                    v-for="tag in item.tags.slice(0, 3)"
                    :key="tag"
                    class="affair-tag"
                    >{{ tag }}</span
                  >
                  <!-- A7：公示期以存证链锚定时刻为准（不用本机时钟冒充链上时间） -->
                  <span
                    v-if="item.publicity"
                    class="affair-card-status affair-status-publicity"
                  >
                    {{ publicityText(item.publicity) }}
                  </span>
                  <span class="affair-card-status affair-status-open"
                    >进行中</span
                  >
                </div>
              </div>
              <span class="affair-card-action">去处理 ›</span>
            </button>
          </div>

          <!-- 已关闭 -->
          <div v-if="grouped.closed.length > 0" class="affair-group">
            <div class="affair-group-title">
              已关闭（{{ grouped.closed.length }}）
            </div>
            <button
              v-for="item in grouped.closed"
              :key="item.affairId"
              type="button"
              class="affair-card affair-card--closed"
              @click="openAffair(item)"
              @touchstart="lp.start($event, item)"
              @touchmove="lp.move"
              @touchend="lp.end"
              @touchcancel="lp.end"
              @contextmenu.prevent="onCardMenu($event, item)"
            >
              <div class="affair-card-main">
                <span class="affair-card-title"
                  ><el-icon v-if="isAffairFavorited(item.affairId)" class="affair-card-fav" :size="13"><StarFilled /></el-icon
                  >{{ item.title }}</span
                >
                <span v-if="item.summary" class="affair-card-summary">{{
                  item.summary
                }}</span>
                <div class="affair-card-meta">
                  <span
                    v-for="tag in item.tags.slice(0, 3)"
                    :key="tag"
                    class="affair-tag"
                    >{{ tag }}</span
                  >
                  <!-- G8：决议 ≠ 已执行——链上决议生效后区分链下执行回报 -->
                  <span
                    v-if="item.exec"
                    class="affair-card-status affair-status-exec"
                  >
                    {{ execText(item.exec) }}
                  </span>
                  <span class="affair-card-status affair-status-closed"
                    >已关闭</span
                  >
                </div>
              </div>
              <span class="affair-card-arrow">›</span>
            </button>
          </div>

          <!-- 空态 -->
          <div
            v-if="
              !loading &&
              grouped.open.length === 0 &&
              grouped.closed.length === 0
            "
            class="affairs-empty"
          >
            <el-empty
              :image-size="100"
              :description="
                hasActiveFilter ? '没有符合筛选的事务' : '暂无与我相关的事务'
              "
            >
              <template #default>
                <p v-if="!hasActiveFilter" class="affairs-empty-hint">
                  我发起 / 我关注 / 需要我处理的事务会聚合在这里。
                </p>
              </template>
            </el-empty>
          </div>
        </template>
      </div>
    </PullRefresh>

    <!-- 长按 / 右键次级动作（M17，G3 弹层体系统一 Element dropdown，virtual-ref 锚定被点卡片）：
         收藏=本机持久化（stores/affairs/affair-favorites，内核无收藏接口、不承诺跨设备同步）；
         复制标题便于转发引用（事务暂无深链文本格式，先复制标题+摘要） -->
    <el-dropdown
      ref="cardMenuRef"
      trigger="contextmenu"
      virtual-triggering
      :virtual-ref="cardMenuAnchor"
      placement="bottom-start"
      popper-class="spark-ctx-popper"
      @command="onCardMenuCommand"
      @visible-change="onCardMenuVisibleChange"
    >
      <span class="affair-menu-anchor" aria-hidden="true" />
      <template #dropdown>
        <el-dropdown-menu>
          <el-dropdown-item command="favorite" :icon="Star">
            {{ cardMenu.item && isAffairFavorited(cardMenu.item.affairId) ? '取消收藏' : '收藏（仅本机）' }}
          </el-dropdown-item>
          <el-dropdown-item command="copy" :icon="DocumentCopy">复制标题</el-dropdown-item>
        </el-dropdown-menu>
      </template>
    </el-dropdown>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, reactive, ref, nextTick } from 'vue';
import { ElMessage, ElMessageBox, type DropdownInstance } from 'element-plus';
import { DocumentCopy, Star, StarFilled } from '@element-plus/icons-vue';
import { isAffairFavorited, toggleAffairFavorite } from '../stores/affairs/affair-favorites';
import { createLongPress } from '../components/mobile-long-press';
import { currentPage } from '../stores/mobile-nav';
import {
  actionableCount,
  affairFeed,
  affairFeedError,
  affairFeedLoading,
  collectAffairTags,
  EMPTY_FILTERS,
  filterAffairs,
  refreshAffairFeed,
  type AffairExecSummary,
  type AffairFeedItem,
  type AffairFilterState,
  type AffairPublicity,
  type AffairRelationFilter,
  type AffairStatusFilter,
} from '../stores/affairs/affair-feed';
import { openAffairInPlugin } from '../stores/affairs/affair-open';
import { currentUser } from '../stores/current-user';
import { isMobileLayout } from '../stores/ui-layout';
import { closeShellModal, openShellModal } from '../stores/shell-modal';
import PullRefresh from '../components/PullRefresh.vue';

const STATUS_OPTIONS: Array<{ key: AffairStatusFilter; label: string }> = [
  { key: 'open', label: '进行中' },
  { key: 'closed', label: '已关闭' },
];
const RELATION_OPTIONS: Array<{ key: AffairRelationFilter; label: string }> = [
  { key: 'mine', label: '我发起' },
  { key: 'following', label: '我关注' },
];

/** 公示期文案（A7）：一律由链上锚定时刻推导；未锚定如实说「未锚定」 */
function publicityText(publicity: AffairPublicity): string {
  if (publicity.anchoredMs === null) {
    return '公示期未锚定（等待存证链确认）';
  }
  const hours =
    publicity.pubPeriodMs >= 3_600_000
      ? `${Math.round(publicity.pubPeriodMs / 3_600_000)} 小时`
      : `${Math.round(publicity.pubPeriodMs / 60_000)} 分钟`;
  return `公示中 · 链上锚定 ${new Date(publicity.anchoredMs).toLocaleString()}（公示期 ${hours}）`;
}

/** 执行回报文案（G8：链上决议 ≠ 链下已执行） */
function execText(exec: AffairExecSummary): string {
  return exec.reported >= exec.total
    ? `执行回报 ${exec.reported}/${exec.total} · 已回报`
    : `链上决议已生效 · 执行回报 ${exec.reported}/${exec.total}（链下执行中）`;
}

export default defineComponent({
  name: 'AffairsPage',
  components: { PullRefresh, StarFilled },
  setup() {
    const loading = computed(() => affairFeedLoading.value);
    const error = computed(() => affairFeedError.value);

    // A6：筛选选中态（chips 行）；组合语义见 affair-feed.filterAffairs
    const filters = reactive<AffairFilterState>({
      ...EMPTY_FILTERS,
      status: [],
      relation: [],
      tags: [],
    });
    const myRootId = computed(() => currentUser.rootId ?? '');

    const filteredFeed = computed(() =>
      filterAffairs(affairFeed.value, filters, myRootId.value),
    );
    const grouped = computed(() => {
      const open = filteredFeed.value.filter((item) => !item.closed);
      const closed = filteredFeed.value.filter((item) => item.closed);
      return { open, closed };
    });

    const allTags = computed(() =>
      collectAffairTags(affairFeed.value).slice(0, 8),
    );
    // 没有任何可筛维度时整条 chips 行不渲染（全空列表不显示筛选器）
    const hasAnyFilterSource = computed(() => affairFeed.value.length > 0);
    const hasActiveFilter = computed(
      () =>
        filters.status.length > 0 ||
        filters.relation.length > 0 ||
        filters.tags.length > 0,
    );

    const toggleIn = (list: string[], key: string) => {
      const index = list.indexOf(key);
      if (index >= 0) {
        list.splice(index, 1);
      } else {
        list.push(key);
      }
    };
    const toggleStatus = (key: AffairStatusFilter) =>
      toggleIn(filters.status, key);
    const toggleRelation = (key: AffairRelationFilter) =>
      toggleIn(filters.relation, key);
    const toggleTag = (tag: string) => toggleIn(filters.tags, tag);
    const clearFilters = () => {
      filters.status = [];
      filters.relation = [];
      filters.tags = [];
    };

    // 返回 Promise：下拉刷新容器据其完成时机收起「正在刷新」指示
    const refresh = () => refreshAffairFeed();

    // M13：再点一次事务 tab（spark:tab-reselect）——栈已被 App.vue 重置；列表回顶并刷新
    const MOBILE_TAB = 'affairs';
    const rootEl = ref<HTMLElement | null>(null);
    const onTabReselect = (event: Event) => {
      if ((event as CustomEvent<string>).detail !== MOBILE_TAB) {
        return;
      }
      if (currentPage(MOBILE_TAB).page !== 'root') {
        return;
      }
      rootEl.value?.querySelector('.affairs-body')?.scrollTo({ top: 0 });
      void refresh();
    };

    // M17：长按（移动端）/ 右键（桌面端）卡片次级动作——收藏（本机）/ 复制标题
    const cardMenu = reactive<{ item: AffairFeedItem | null }>({ item: null });
    const cardMenuAnchor = ref<HTMLElement | null>(null);
    const cardMenuRef = ref<DropdownInstance | null>(null);

    const openCardMenu = (anchor: HTMLElement | null, item: AffairFeedItem) => {
      if (!anchor) {
        return;
      }
      cardMenu.item = item;
      cardMenuAnchor.value = anchor;
      // 等 anchor 更新后再开（首次打开时 virtual-ref 尚未指向目标卡片）
      void nextTick(() => cardMenuRef.value?.handleOpen());
    };
    const onCardMenu = (event: MouseEvent, item: AffairFeedItem) =>
      openCardMenu(event.currentTarget as HTMLElement, item);
    const lp = createLongPress<AffairFeedItem>((item, event) =>
      openCardMenu((event.target as HTMLElement).closest('.affair-card'), item)
    );
    const onCardMenuVisibleChange = (visible: boolean) => {
      if (!visible) {
        cardMenu.item = null;
      }
    };
    const onCardMenuCommand = (command: string) => {
      const item = cardMenu.item;
      cardMenu.item = null;
      if (!item) {
        return;
      }
      if (command === 'favorite') {
        const favorited = toggleAffairFavorite(item.affairId);
        ElMessage.success(favorited ? '已收藏（仅本机）' : '已取消收藏');
      } else if (command === 'copy') {
        const text = item.summary ? `${item.title}\n${item.summary}` : item.title;
        void navigator.clipboard
          .writeText(text)
          .then(() => ElMessage.success('已复制'))
          .catch(() => ElMessage.error('复制失败'));
      }
    };

    /** 点卡片：按类型分发到类型插件视图（3.4；当前经 affair-open 打开事务插件详情）。
        A8：本机没有承接插件时引导去应用市场（不静默失败） */
    const openAffair = async (item: AffairFeedItem) => {
      const result = await openAffairInPlugin(item);
      if (result.ok) {
        return;
      }
      try {
        await ElMessageBox.confirm(
          '本机还没有能处理这类事务的应用。可前往应用市场安装事务类型插件后再打开。',
          '未安装承接应用',
          {
            confirmButtonText: '去应用市场',
            cancelButtonText: '取消',
            type: 'info',
          },
        );
      } catch {
        return; // 用户取消
      }
      if (isMobileLayout.value) {
        window.dispatchEvent(
          new CustomEvent('spark:switch-tab', { detail: 'apps' }),
        );
      } else {
        closeShellModal();
        openShellModal('apps', { appsView: 'market' });
      }
    };

    onMounted(refresh);
    onMounted(() => window.addEventListener('spark:tab-reselect', onTabReselect));
    onBeforeUnmount(() => window.removeEventListener('spark:tab-reselect', onTabReselect));

    return {
      grouped,
      actionableCount,
      loading,
      error,
      refresh,
      openAffair,
      filters,
      STATUS_OPTIONS,
      RELATION_OPTIONS,
      allTags,
      hasAnyFilterSource,
      hasActiveFilter,
      toggleStatus,
      toggleRelation,
      toggleTag,
      clearFilters,
      publicityText,
      execText,
      rootEl,
      lp,
      cardMenu,
      cardMenuAnchor,
      cardMenuRef,
      isAffairFavorited,
      onCardMenu,
      onCardMenuCommand,
      onCardMenuVisibleChange,
      Star,
      StarFilled,
      DocumentCopy,
    };
  },
});
</script>

<style scoped>
.affairs-page {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page);
}

/* 待处理计数 chip（M7：原头部计数并入列表区顶部，样式沿用原头部计数徽章） */
.affairs-chip-row {
  margin-bottom: var(--spark-gap-page);
}

.affairs-count {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-danger);
  background: var(--spark-danger-bg);
  padding: 2px 8px;
  border-radius: var(--spark-radius-s);
}

.affairs-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: var(--spark-gap-page) var(--spark-padding-page);
}

.affairs-error {
  padding: 20px;
  text-align: center;
  color: var(--spark-text-3);
  font-size: var(--spark-font-size-secondary);
}

.affair-group {
  margin-bottom: var(--spark-gap-page);
}

.affair-group-title {
  font-size: var(--spark-font-size-secondary);
  font-weight: 600;
  color: var(--spark-text-2);
  margin-bottom: 8px;
  padding-left: 2px;
}

.affair-group-title--action {
  color: var(--spark-primary);
}

.affair-card {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 12px 14px;
  margin-bottom: 8px;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  background: var(--spark-bg-card);
  box-shadow: var(--spark-shadow-card);
  cursor: pointer;
  text-align: left;
  font-family: inherit;
  transition: box-shadow var(--spark-dur-fast) var(--spark-ease-standard);
}

.affair-card:hover {
  box-shadow: var(--spark-shadow-hover);
}

.affair-card--open {
  border-left: 3px solid var(--spark-primary);
}

.affair-card--closed {
  opacity: 0.75;
}

.affair-card-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.affair-card-title {
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* M17 收藏星标：标题前小星，弱强调 */
.affair-card-fav {
  color: var(--spark-warning);
  margin-right: 4px;
  vertical-align: -1px;
}

.affair-card-summary {
  font-size: var(--spark-font-size-placeholder);
  color: var(--spark-text-2);
  overflow: hidden;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}

.affair-card-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.affair-tag {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  background: var(--spark-bg-hover);
  padding: 1px 6px;
  border-radius: var(--spark-radius-s);
}

.affair-card-status {
  font-size: var(--spark-font-size-secondary);
  padding: 1px 6px;
  border-radius: var(--spark-radius-s);
}

.affair-status-open {
  color: var(--spark-success);
  background: var(--spark-success-bg);
}

/* A7 公示期（链上锚定口径）：与「进行中」区分的中性提示色 */
.affair-status-publicity {
  color: var(--spark-warning);
  background: var(--spark-warning-bg);
}

/* G8 执行回报：链上决议 ≠ 链下执行，弱一级中性色 */
.affair-status-exec {
  color: var(--spark-text-2);
  background: var(--spark-bg-hover);
}

/* A6 筛选 chips 行：可组合，选中态高亮 */
.affairs-filters {
  margin-bottom: var(--spark-gap-page);
}

.affairs-filter-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.affair-filter-chip {
  border: 1px solid var(--spark-border-light);
  border-radius: 999px;
  background: var(--spark-bg-card);
  color: var(--spark-text-2);
  font-size: var(--spark-font-size-secondary);
  font-family: inherit;
  padding: 3px 10px;
  cursor: pointer;
}

.affair-filter-chip:hover {
  background: var(--spark-bg-hover);
}

.affair-filter-chip.active {
  color: var(--spark-primary);
  border-color: var(--spark-primary);
  background: var(--spark-primary-light);
}

.affair-filter-clear {
  color: var(--spark-text-3);
}

.affair-status-closed {
  color: var(--spark-text-3);
  background: var(--spark-bg-hover);
}

.affair-card-action {
  flex-shrink: 0;
  font-size: var(--spark-font-size-placeholder);
  color: var(--spark-primary);
  font-weight: 600;
}

.affair-card-arrow {
  flex-shrink: 0;
  color: var(--spark-text-3);
  font-size: 18px;
}

.affairs-empty {
  padding: 40px 0;
}

.affairs-empty-hint {
  margin: 0;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}
</style>
