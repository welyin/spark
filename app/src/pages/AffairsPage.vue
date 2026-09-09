<!-- 事务页（阶段 3 / README §4.4、shell-desktop §2.4）：跨全部域「与我相关」的事务列表。
     「等我操作」（进行中）置顶高亮分组，已关闭在后；只做与我相关，公共议题是空间插件（README §八决策 2）。
     点卡片按事务类型打开对应类型插件视图（3.4 deep-link 承接；当前版本先跳转到事务详情插件）。
     双端：PC 三栏（筛选 | 列表 | 插件视图）为终态，第一版先落地列表 + 点击分发；
     移动端列表 + 去处理大按钮（shell-mobile §四）。 -->
<template>
  <section class="affairs-page">
    <!-- 顶部：标题 + 筛选入口（筛选维度收进抽屉，第一版先放刷新与计数） -->
    <header class="affairs-header">
      <h1 class="affairs-title">事务</h1>
      <span v-if="actionableCount > 0" class="affairs-count">待我处理 {{ actionableCount }}</span>
      <button type="button" class="affairs-refresh" title="刷新" @click="refresh">
        <el-icon :size="16" :class="{ 'is-loading': loading }"><Refresh /></el-icon>
      </button>
    </header>

    <div class="affairs-body">
      <div v-if="error" class="affairs-error">{{ error }}</div>

      <template v-else>
        <!-- 等我操作（进行中）置顶高亮分组 -->
        <div v-if="grouped.open.length > 0" class="affair-group">
          <div class="affair-group-title affair-group-title--action">等我操作（{{ grouped.open.length }}）</div>
          <button
            v-for="item in grouped.open"
            :key="item.affairId"
            type="button"
            class="affair-card affair-card--open"
            @click="openAffair(item)"
          >
            <div class="affair-card-main">
              <span class="affair-card-title">{{ item.title }}</span>
              <span v-if="item.summary" class="affair-card-summary">{{ item.summary }}</span>
              <div class="affair-card-meta">
                <span v-for="tag in item.tags.slice(0, 3)" :key="tag" class="affair-tag">{{ tag }}</span>
                <span class="affair-card-status affair-status-open">进行中</span>
              </div>
            </div>
            <span class="affair-card-action">去处理 ›</span>
          </button>
        </div>

        <!-- 已关闭 -->
        <div v-if="grouped.closed.length > 0" class="affair-group">
          <div class="affair-group-title">已关闭（{{ grouped.closed.length }}）</div>
          <button
            v-for="item in grouped.closed"
            :key="item.affairId"
            type="button"
            class="affair-card affair-card--closed"
            @click="openAffair(item)"
          >
            <div class="affair-card-main">
              <span class="affair-card-title">{{ item.title }}</span>
              <span v-if="item.summary" class="affair-card-summary">{{ item.summary }}</span>
              <div class="affair-card-meta">
                <span v-for="tag in item.tags.slice(0, 3)" :key="tag" class="affair-tag">{{ tag }}</span>
                <span class="affair-card-status affair-status-closed">已关闭</span>
              </div>
            </div>
            <span class="affair-card-arrow">›</span>
          </button>
        </div>

        <!-- 空态 -->
        <div v-if="!loading && grouped.open.length === 0 && grouped.closed.length === 0" class="affairs-empty">
          <el-empty :image-size="100" description="暂无与我相关的事务">
            <template #default>
              <p class="affairs-empty-hint">我发起 / 我关注 / 需要我处理的事务会聚合在这里。</p>
            </template>
          </el-empty>
        </div>
      </template>
    </div>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted } from 'vue';
import { Refresh } from '@element-plus/icons-vue';
import {
  actionableCount,
  affairFeedError,
  affairFeedLoading,
  groupedFeed,
  refreshAffairFeed,
  type AffairFeedItem
} from '../stores/affairs/affair-feed';
import { openAffairInPlugin } from '../stores/affairs/affair-open';

export default defineComponent({
  name: 'AffairsPage',
  components: { Refresh },
  setup() {
    const loading = computed(() => affairFeedLoading.value);
    const error = computed(() => affairFeedError.value);

    const refresh = () => {
      void refreshAffairFeed();
    };

    /** 点卡片：按类型分发到类型插件视图（3.4；当前经 affair-open 打开事务插件详情） */
    const openAffair = (item: AffairFeedItem) => {
      openAffairInPlugin(item);
    };

    onMounted(refresh);

    return { grouped: groupedFeed, actionableCount, loading, error, refresh, openAffair };
  }
});
</script>

<style scoped>
.affairs-page {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page);
}

.affairs-header {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px var(--spark-padding-page);
  background: var(--spark-bg-card);
  border-bottom: 1px solid var(--spark-border-light);
}

.affairs-title {
  margin: 0;
  font-size: var(--spark-font-size-title);
  font-weight: 600;
  color: var(--spark-text-1);
}

.affairs-count {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-danger);
  background: var(--spark-danger-bg);
  padding: 2px 8px;
  border-radius: var(--spark-radius-s);
}

.affairs-refresh {
  margin-left: auto;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  border: 0;
  border-radius: var(--spark-radius-m);
  background: transparent;
  color: var(--spark-text-2);
  cursor: pointer;
}

.affairs-refresh:hover {
  background: var(--spark-bg-hover);
}

.affairs-body {
  flex: 1;
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
