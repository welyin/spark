<template>
  <div class="apps-market">
    <!-- 固定头部（滚动不随动）：大标题居中（返回按钮左浮）+ 收录/探索/开发者分区 + 收录搜索与分类页签 -->
    <div class="market-head">
      <header class="apps-market-header">
        <el-button text :icon="ArrowLeft" class="market-back" @click="emit('back')">返回</el-button>
        <h1 class="apps-title market-title">应用市场</h1>
      </header>

      <!-- 默认视图 = 收录层（官方收录 + 组织白名单）；探索页 = 已验证全网广播；
           开发者页 = 我发布过的应用 + 发布新应用入口（plugin-dist §8，阶段 C 波次 4） -->
      <el-radio-group v-model="marketTab" class="market-view-switch">
        <el-radio-button value="collection">收录</el-radio-button>
        <el-radio-button value="explore">探索</el-radio-button>
        <el-radio-button value="developer">开发者</el-radio-button>
      </el-radio-group>

      <template v-if="marketTab === 'collection'">
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
      </template>
    </div>

    <!-- 发布声明对话框（plugin-dist §8，开发者标签页「发布新应用」入口）：
         解析声明 → 算 PoW → 广播 -->
    <el-dialog v-model="announceDialogVisible" title="发布插件声明（开发者）" width="480">
      <div class="repo-install-dialog">
        <el-input
          v-model="announceIdInput"
          placeholder="你的插件仓库地址，如 github.com/owner/repo"
          clearable
          @keyup.enter="resolveAnnounce"
        >
          <template #append>
            <el-button :loading="announceResolving" @click="resolveAnnounce">解析</el-button>
          </template>
        </el-input>
        <el-alert v-if="announceError" :title="announceError" type="error" :closable="false" show-icon />
        <el-alert
          v-if="announceDone"
          title="声明已广播（PoW 已计算并签名），其他节点收到后将懒惰核查仓库声明文件"
          type="success"
          :closable="false"
          show-icon
        />
        <div v-if="announcePreview" class="repo-preview">
          <div class="repo-preview-head">
            <img v-if="announcePreview.icon" :src="announcePreview.icon" class="repo-preview-icon" alt="" />
            <span v-else class="repo-preview-icon repo-preview-icon-fallback">{{ announcePreview.name.slice(0, 1) }}</span>
            <div>
              <h3>{{ announcePreview.name }} <el-tag size="small" effect="plain">v{{ announcePreview.version }}</el-tag></h3>
              <p class="repo-preview-id">{{ announcePreview.id }}</p>
            </div>
          </div>
          <p class="repo-preview-summary">{{ announcePreview.summary }}</p>
        </div>
      </div>
      <template #footer>
        <el-button @click="announceDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="!announcePreview" :loading="announcePublishing" @click="confirmAnnounce">
          {{ announcePublishing ? '计算 PoW 并广播中…' : '广播声明' }}
        </el-button>
      </template>
    </el-dialog>

    <!-- 探索页：已验证全网广播（随机排序 + 换一批 + 搜索直达） -->
    <AppExplorePanel
      v-if="marketTab === 'explore'"
      :installed-ids="installedIds"
      @install-repo="(declaration) => emit('install-repo', declaration)"
    />

    <!-- 开发者页：我发布过的应用（本地索引 publisher==我的 rootId）+ 顶部「发布新应用」入口 -->
    <template v-else-if="marketTab === 'developer'">
      <div class="market-developer-toolbar">
        <el-button type="primary" size="small" @click="openAnnounceDialog">发布新应用</el-button>
      </div>
      <el-empty
        v-if="myAnnounces.length === 0"
        description="还没有发布过应用，点击上方「发布新应用」广播你的第一个插件声明"
      />
      <div v-else class="market-grid market-developer-grid">
        <div v-for="entry in myAnnounces" :key="entry.announce.id" class="market-card">
          <img
            v-if="announceDisplayIcon(entry)"
            :src="announceDisplayIcon(entry)"
            class="market-card-icon explore-card-icon"
            alt=""
          />
          <span v-else class="market-card-icon" :style="{ background: developerIconBackground(entry) }">
            {{ announceDisplayName(entry).slice(0, 1) }}
          </span>
          <div class="market-card-info">
            <h3>
              {{ announceDisplayName(entry) }}
              <span class="explore-card-version">v{{ announceDisplayVersion(entry) }}</span>
            </h3>
            <p>{{ announceDisplaySummary(entry) }}</p>
            <div class="market-card-tags">
              <el-tag size="small" effect="plain">{{ announceCategoryLabel(entry.announce.category) }}</el-tag>
              <!-- 核查状态（plugin-dist §8.7 懒惰核查）：自己发布的未过核查条目也如实展示 -->
              <el-tag size="small" :type="verifiedTagType(entry.verified)">{{ verifiedLabel(entry.verified) }}</el-tag>
            </div>
          </div>
        </div>
      </div>
    </template>

    <template v-else>
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
                  <el-tag v-if="isMockApp(item)" size="small" type="info">演示数据</el-tag>
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
                  <el-tag v-if="isMockApp(item)" size="small" type="info">演示数据</el-tag>
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

      <!-- 收录层 · 组织白名单（plugin_system.md「市场展示与排序」默认视图组成之一）：
           数据结构预留，来源待组织管理接口（管理员推荐清单），先展示分组占位 -->
      <section v-if="activeCategory === '全部' && !keyword.trim()" class="market-section">
        <h2 class="market-section-title">组织白名单</h2>
        <div v-if="orgWhitelistItems.length > 0" class="market-grid">
          <div
            v-for="item in orgWhitelistItems"
            :key="`org-${item.id}`"
            class="market-card"
            @click="emit('detail', item)"
          >
            <span class="market-card-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
            <div class="market-card-info">
              <h3>{{ item.name }}</h3>
              <p>{{ item.description }}</p>
            </div>
            <div class="market-card-action">
              <el-button v-if="item.installed" size="small" disabled>已安装</el-button>
              <el-button v-else size="small" type="primary" @click.stop="emit('install', item)">安装</el-button>
            </div>
          </div>
        </div>
        <p v-else class="market-org-whitelist-empty">暂无组织推荐应用（由组织管理员推荐后展示）</p>
      </section>
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ArrowLeft, Search } from '@element-plus/icons-vue';
import type {
  PluginAnnounceIndexEntryDto,
  PluginMarketItemDto,
  RepoPluginDeclarationDto
} from '../../api/types';
import { isMockApp } from '../../mock/apps';
import { currentUser } from '../../stores/current-user';
import { hashGradient } from '../../utils/palette';
import { MARKET_CATEGORIES, appIconBackground, marketCategoryOf, marketItemMatches } from './apps-store';
import {
  announceCategoryLabel,
  announceDisplayIcon,
  announceDisplayName,
  announceDisplaySummary,
  announceDisplayVersion,
  filterMyAnnounces,
  sortAnnouncesByUpdated
} from './apps-explore';
import AppExplorePanel from './AppExplorePanel.vue';

export default defineComponent({
  name: 'AppMarketPanel',
  components: { AppExplorePanel },
  props: {
    items: { type: Array as PropType<PluginMarketItemDto[]>, required: true }
  },
  emits: ['back', 'detail', 'install', 'install-repo'],
  setup(props, { emit }) {
    // 市场分区（plugin_system.md「市场展示与排序」）：默认收录层，探索页为已验证全网广播，
    // 开发者页为我发布过的应用（阶段 C 波次 4）
    const marketTab = ref<'collection' | 'explore' | 'developer'>('collection');
    const keyword = ref('');
    const activeCategory = ref<string>('全部');

    // TODO(mock): 组织白名单数据结构预留——来源待组织管理接口（管理员推荐清单下发），先空清单占位
    const orgWhitelistItems = ref<PluginMarketItemDto[]>([]);

    /** 已安装 id 清单（探索页「已安装」标记用） */
    const installedIds = computed(() =>
      props.items.filter((item) => item.installed).map((item) => item.id)
    );

    // 发布声明（plugin-dist §8，开发者标签页「发布新应用」入口）：解析 spark-plugin.json 预填 →
    // 内核签名 + 算 PoW（秒级）→ 广播；声明内容以仓库声明文件为准（id 一致性已由
    // resolveRepo 校验），信任锚不变
    const announceDialogVisible = ref(false);
    const announceIdInput = ref('');
    const announceResolving = ref(false);
    const announcePublishing = ref(false);
    const announceError = ref('');
    const announceDone = ref(false);
    const announcePreview = ref<RepoPluginDeclarationDto | null>(null);

    const openAnnounceDialog = () => {
      announceIdInput.value = '';
      announceError.value = '';
      announceDone.value = false;
      announcePreview.value = null;
      announceDialogVisible.value = true;
    };

    const resolveAnnounce = async () => {
      const id = announceIdInput.value.trim();
      if (!id) {
        return;
      }
      announceResolving.value = true;
      announceError.value = '';
      announceDone.value = false;
      announcePreview.value = null;
      try {
        announcePreview.value = await window.electronAPI.pluginMarket.resolveRepo(id);
      } catch (error) {
        announceError.value = `解析失败：${error}`;
      } finally {
        announceResolving.value = false;
      }
    };

    /** 发布声明的 releaseUrl：按 §2.2 tag 规则从声明 id/version 推导
     *  （根仓库 v<version>，monorepo <末段>-v<version>） */
    const announceReleaseUrl = (declaration: RepoPluginDeclarationDto): string => {
      const segments = declaration.id.split('/');
      const base = segments.slice(0, 3).join('/');
      const tag =
        segments.length > 3
          ? `${segments[segments.length - 1]}-v${declaration.version}`
          : `v${declaration.version}`;
      return `https://${base}/releases/tag/${tag}`;
    };

    const confirmAnnounce = async () => {
      const declaration = announcePreview.value;
      if (!declaration) {
        return;
      }
      announcePublishing.value = true;
      announceError.value = '';
      announceDone.value = false;
      try {
        await window.electronAPI.pluginMarket.announcePublish({
          id: declaration.id,
          name: declaration.name,
          icon: declaration.icon,
          summary: declaration.summary,
          category: declaration.category,
          version: declaration.version,
          releaseUrl: announceReleaseUrl(declaration)
        });
        announceDone.value = true;
        // 广播成功后刷新「我发布过的应用」清单（新条目以核查中状态入账）
        void loadMyAnnounces();
      } catch (error) {
        announceError.value = `广播失败：${error}`;
      } finally {
        announcePublishing.value = false;
      }
    };

    // ---- 开发者标签页：我发布过的应用（本地索引 publisher==我的 rootId，经 announceList 过滤） ----
    const myAnnounces = ref<PluginAnnounceIndexEntryDto[]>([]);

    const loadMyAnnounces = async () => {
      try {
        const entries = await window.electronAPI.pluginMarket.announceList();
        // 展示序：updatedAt 降序稳定序（与探索页搜索直达同口径）
        myAnnounces.value = sortAnnouncesByUpdated(
          filterMyAnnounces(entries ?? [], currentUser.rootId ?? '')
        );
      } catch {
        // 索引读取失败保留当前清单，不阻断浏览
      }
    };

    // 切到开发者标签页时拉取一次（离开再回来会重拉，保持与本地索引同步）
    watch(marketTab, (tab) => {
      if (tab === 'developer') {
        void loadMyAnnounces();
      }
    });

    /** 核查状态展示（plugin-dist §8.7）：verified/pending/failed → 标签文案与类型 */
    const verifiedLabel = (verified: PluginAnnounceIndexEntryDto['verified']): string =>
      verified === 'verified' ? '已验证' : verified === 'pending' ? '核查中' : '未通过';
    const verifiedTagType = (verified: PluginAnnounceIndexEntryDto['verified']): 'success' | 'info' | 'danger' =>
      verified === 'verified' ? 'success' : verified === 'pending' ? 'info' : 'danger';

    const developerIconBackground = (entry: PluginAnnounceIndexEntryDto) =>
      hashGradient(entry.announce.id || entry.announce.name);

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
      marketTab,
      keyword,
      activeCategory,
      categoryTabs,
      filteredItems,
      featuredItems,
      groupedItems,
      orgWhitelistItems,
      installedIds,
      marketCategoryOf,
      appIconBackground,
      isMockApp,
      announceDialogVisible,
      announceIdInput,
      announceResolving,
      announcePublishing,
      announceError,
      announceDone,
      announcePreview,
      openAnnounceDialog,
      resolveAnnounce,
      confirmAnnounce,
      myAnnounces,
      verifiedLabel,
      verifiedTagType,
      developerIconBackground,
      announceCategoryLabel,
      announceDisplayIcon,
      announceDisplayName,
      announceDisplaySummary,
      announceDisplayVersion,
      ArrowLeft,
      Search,
      emit
    };
  }
});
</script>
