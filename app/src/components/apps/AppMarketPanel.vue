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

      <!-- 仓库锚定安装入口（plugin-dist）：输入仓库地址 → 解析声明文件 → 确认安装 -->
      <div class="market-repo-entry">
        <el-button size="small" @click="openRepoDialog">按仓库地址安装</el-button>
        <!-- 广播索引发布入口（plugin-dist §8，开发者模式）：解析声明 → 算 PoW → 广播 -->
        <el-button size="small" @click="openAnnounceDialog">发布声明（开发者）</el-button>
      </div>

      <el-tabs v-model="activeCategory" class="market-tabs">
        <el-tab-pane v-for="category in categoryTabs" :key="category" :label="category" :name="category" />
      </el-tabs>
    </div>

    <el-dialog v-model="repoDialogVisible" title="按仓库地址安装" width="480">
      <div class="repo-install-dialog">
        <el-input
          v-model="repoIdInput"
          placeholder="如 github.com/owner/repo（支持 gitlab.com / gitee.com）"
          clearable
          @keyup.enter="resolveRepo"
        >
          <template #append>
            <el-button :loading="repoResolving" @click="resolveRepo">解析</el-button>
          </template>
        </el-input>
        <el-alert v-if="repoError" :title="repoError" type="error" :closable="false" show-icon />
        <div v-if="repoPreview" class="repo-preview">
          <div class="repo-preview-head">
            <img v-if="repoPreview.icon" :src="repoPreview.icon" class="repo-preview-icon" alt="" />
            <span v-else class="repo-preview-icon repo-preview-icon-fallback">{{ repoPreview.name.slice(0, 1) }}</span>
            <div>
              <h3>{{ repoPreview.name }} <el-tag size="small" effect="plain">v{{ repoPreview.version }}</el-tag></h3>
              <p class="repo-preview-id">{{ repoPreview.id }}</p>
            </div>
          </div>
          <p class="repo-preview-summary">{{ repoPreview.summary }}</p>
          <p v-if="repoPreview.permissions.length > 0" class="repo-preview-permissions">
            声明权限：{{ repoPreview.permissions.join('、') }}
          </p>
        </div>
      </div>
      <template #footer>
        <el-button @click="repoDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="!repoPreview" @click="confirmRepoInstall">确认安装</el-button>
      </template>
    </el-dialog>

    <!-- 发布声明对话框（plugin-dist §8，开发者模式）：解析声明文件 → 确认 → 算 PoW 广播 -->
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
import type { PluginMarketItemDto, RepoPluginDeclarationDto } from '../../api/types';
import { MARKET_CATEGORIES, appIconBackground, marketCategoryOf, marketItemMatches } from './apps-store';

export default defineComponent({
  name: 'AppMarketPanel',
  props: {
    items: { type: Array as PropType<PluginMarketItemDto[]>, required: true }
  },
  emits: ['back', 'detail', 'install', 'install-repo'],
  setup(props, { emit }) {
    const keyword = ref('');
    const activeCategory = ref<string>('全部');

    // 仓库锚定安装（plugin-dist）：解析 spark-plugin.json 预览，确认后由父组件安装
    const repoDialogVisible = ref(false);
    const repoIdInput = ref('');
    const repoResolving = ref(false);
    const repoError = ref('');
    const repoPreview = ref<RepoPluginDeclarationDto | null>(null);

    const openRepoDialog = () => {
      repoIdInput.value = '';
      repoError.value = '';
      repoPreview.value = null;
      repoDialogVisible.value = true;
    };

    const resolveRepo = async () => {
      const id = repoIdInput.value.trim();
      if (!id) {
        return;
      }
      repoResolving.value = true;
      repoError.value = '';
      repoPreview.value = null;
      try {
        repoPreview.value = await window.electronAPI.pluginMarket.resolveRepo(id);
      } catch (error) {
        repoError.value = `解析失败：${error}`;
      } finally {
        repoResolving.value = false;
      }
    };

    const confirmRepoInstall = () => {
      if (!repoPreview.value) {
        return;
      }
      const declaration = repoPreview.value;
      repoDialogVisible.value = false;
      emit('install-repo', declaration);
    };

    // 发布声明（plugin-dist §8，开发者模式）：解析 spark-plugin.json 预填 →
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
          releaseUrl: ''
        });
        announceDone.value = true;
      } catch (error) {
        announceError.value = `广播失败：${error}`;
      } finally {
        announcePublishing.value = false;
      }
    };

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
      repoDialogVisible,
      repoIdInput,
      repoResolving,
      repoError,
      repoPreview,
      openRepoDialog,
      resolveRepo,
      confirmRepoInstall,
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
      ArrowLeft,
      Search,
      emit
    };
  }
});
</script>
