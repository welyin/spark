<template>
  <div class="apps-market">
    <!-- 固定头部（滚动不随动）：大标题居中（返回按钮左浮）+ 搜索条 + 分类页签 -->
    <div class="market-head">
      <header class="apps-market-header">
        <el-button text :icon="ArrowLeft" class="market-back" @click="emit('back')">返回</el-button>
        <h1 class="apps-title market-title">应用市场</h1>
      </header>
      <el-input
        v-model="keyword"
        class="apps-search market-search"
        placeholder="搜索名称 / 简介 / 开发者"
        clearable
        :prefix-icon="Search"
      />

      <el-tabs v-model="activeCategory" class="market-tabs">
        <el-tab-pane v-for="category in categoryTabs" :key="category" :label="category" :name="category" />
      </el-tabs>
    </div>

    <el-empty v-if="filteredItems.length === 0" description="没有匹配的应用" />

    <template v-else>
      <!-- 推荐区：取列表前 2 个大图卡片（ui-apps-market §3.2），与下方分类卡片样式拉开差异 -->
      <section v-if="featuredItems.length > 0" class="market-section">
        <h2 class="market-section-title">推荐应用</h2>
        <div class="market-featured">
          <div
            v-for="item in featuredItems"
            :key="`featured-${item.id}`"
            class="market-featured-card"
            @click="emit('detail', item)"
          >
            <span class="market-featured-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
            <div class="market-featured-info">
              <h3>{{ item.name }}</h3>
              <p>{{ item.description }}</p>
              <div class="market-card-tags">
                <el-tag size="small" effect="plain">{{ marketCategoryOf(item) }}</el-tag>
                <el-tag v-if="item.installed" size="small" type="success">已安装</el-tag>
                <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
              </div>
            </div>
            <div class="market-card-action">
              <el-button v-if="item.installed" size="small" disabled>已安装</el-button>
              <el-button v-else size="small" type="primary" @click.stop="emit('install', item)">安装</el-button>
            </div>
          </div>
        </div>
      </section>

      <section v-for="section in groupedItems" :key="section.category" class="market-section">
        <h2 class="market-section-title">{{ section.category }}应用</h2>
        <div class="market-grid">
          <div
            v-for="item in section.items"
            :key="item.id"
            class="market-card"
            @click="emit('detail', item)"
          >
            <span class="market-card-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
            <div class="market-card-info">
              <h3>{{ item.name }}</h3>
              <p>{{ item.description }}</p>
              <div class="market-card-tags">
                <el-tag size="small" effect="plain">{{ marketCategoryOf(item) }}</el-tag>
                <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
              </div>
            </div>
            <div class="market-card-action">
              <el-button v-if="item.installed" size="small" disabled>已安装</el-button>
              <el-button v-else size="small" type="primary" @click.stop="emit('install', item)">安装</el-button>
            </div>
          </div>
        </div>
      </section>
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ArrowLeft, Search } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { MARKET_CATEGORIES, appIconBackground, marketCategoryOf, marketItemMatches } from './apps-store';

export default defineComponent({
  name: 'AppMarketPanel',
  props: {
    items: { type: Array as PropType<PluginMarketItemDto[]>, required: true }
  },
  emits: ['back', 'detail', 'install'],
  setup(props, { emit }) {
    const keyword = ref('');
    const activeCategory = ref<string>('全部');

    const categoryTabs = ['全部', ...MARKET_CATEGORIES] as const;

    const filteredItems = computed(() =>
      props.items.filter(
        (item) =>
          marketItemMatches(item, keyword.value) &&
          (activeCategory.value === '全部' || marketCategoryOf(item) === activeCategory.value)
      )
    );

    // 推荐区：取列表前 2 个（仅在「全部」分类且未搜索时展示）
    const featuredItems = computed(() =>
      activeCategory.value === '全部' && !keyword.value.trim()
        ? filteredItems.value.slice(0, 2)
        : []
    );

    const groupedItems = computed(() => {
      const categories =
        activeCategory.value === '全部' ? MARKET_CATEGORIES : [activeCategory.value];
      return categories
        .map((category) => ({
          category,
          items: filteredItems.value.filter((item) => marketCategoryOf(item) === category)
        }))
        .filter((section) => section.items.length > 0);
    });

    return {
      keyword,
      activeCategory,
      categoryTabs,
      filteredItems,
      featuredItems,
      groupedItems,
      marketCategoryOf,
      appIconBackground,
      ArrowLeft,
      Search,
      emit
    };
  }
});
</script>
