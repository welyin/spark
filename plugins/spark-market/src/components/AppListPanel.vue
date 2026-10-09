<!-- 应用清单（系统层应用管理，移植自壳层 AppListPanel）：
     平铺全部已安装应用，搜索 / 筛选 / 排序 + 头部「+ 安装应用」下拉
     （市场 / 仓库 / 侧载）。卡片点击＝进详情（管理界面不直接启动应用）。
     插件版差异：无 mock/dev 条目；分组/最近使用等壳层 localStorage mock 面
     不在插件内（壳层自身也已从本面板移除分组展示）。 -->
<template>
  <div class="apps-list">
    <header class="apps-list-header">
      <el-input v-model="keyword" class="apps-search" placeholder="搜索应用" clearable :prefix-icon="Search" />
      <el-select v-model="filter" class="apps-filter" size="small">
        <el-option label="全部" value="all" />
        <el-option label="可更新" value="updatable" />
        <el-option label="个人空间" value="personal" />
        <el-option label="组织空间" value="org" />
      </el-select>
      <el-select v-model="sort" class="apps-sort" size="small">
        <el-option label="默认" value="default" />
        <el-option label="名称" value="name" />
        <el-option label="可更新" value="updatable" />
      </el-select>
      <el-dropdown trigger="click" class="app-install-dropdown" @command="handleInstallCommand">
        <el-button type="primary" class="app-install-btn">
          + 安装应用
          <el-icon class="el-icon--right"><ArrowDown /></el-icon>
        </el-button>
        <template #dropdown>
          <el-dropdown-menu class="app-install-menu">
            <el-dropdown-item command="market" class="app-install-menu__market">
              <el-icon :size="18"><Shop /></el-icon>
              <span class="app-install-menu__label">应用商店</span>
              <span class="app-store-new-badge">NEW</span>
            </el-dropdown-item>
            <el-dropdown-item command="repo" divided>
              <el-icon :size="16"><Download /></el-icon>
              <span class="app-install-menu__label">从仓库安装</span>
            </el-dropdown-item>
            <el-dropdown-item command="sideload">
              <el-icon :size="16"><Upload /></el-icon>
              <span class="app-install-menu__label">导入 .spkg 文件</span>
            </el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>
      <AppInstallTools
        ref="installToolsRef"
        :hide-buttons="true"
        @install-repo="(declaration) => emit('install-repo', declaration)"
        @sideloaded="emit('sideloaded')"
      />
    </header>

    <el-empty v-if="installedItems.length === 0" description="还没有安装应用，去市场看看吧">
      <el-button type="primary" @click="onAddApp">进入应用市场</el-button>
    </el-empty>

    <template v-else>
      <div class="app-cards">
        <div v-for="item in visibleItems" :key="item.id" class="app-card" @click="onCardDetail(item)">
          <AppIcon class="app-card-icon" :item="item" />
          <div class="app-card-body">
            <div class="app-card-head">
              <span class="app-card-name">{{ item.name }}</span>
              <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
            </div>
            <p class="app-card-desc">{{ item.description }}</p>
          </div>
          <button type="button" class="app-card-more-btn" title="查看详情" @click.stop="onCardDetail(item)">
            <el-icon :size="18"><MoreFilled /></el-icon>
          </button>
        </div>
      </div>

      <el-empty
        v-if="(keyword.trim() || filter !== 'all') && visibleItems.length === 0"
        description="没有匹配的应用"
      />
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ArrowDown, Download, MoreFilled, Search, Shop, Upload } from '@element-plus/icons-vue';
import type { PluginMarketItem } from '../../../../packages/plugin-sdk/src';
import AppIcon from './AppIcon.vue';
import AppInstallTools from './AppInstallTools.vue';

type FilterKey = 'all' | 'updatable' | 'personal' | 'org';
type SortKey = 'default' | 'name' | 'updatable';

export default defineComponent({
  name: 'AppListPanel',
  components: { ArrowDown, Download, MoreFilled, Search, Shop, Upload, AppInstallTools, AppIcon },
  props: {
    installedItems: { type: Array as PropType<PluginMarketItem[]>, required: true },
    busyByPlugin: { type: Object as PropType<Record<string, string>>, default: () => ({}) }
  },
  emits: ['detail', 'add-app', 'install-repo', 'sideloaded'],
  setup(props, { emit }) {
    const keyword = ref('');
    const filter = ref<FilterKey>('all');
    const sort = ref<SortKey>('default');
    const installToolsRef = ref<InstanceType<typeof AppInstallTools> | null>(null);

    /** supportedSpaces 适用域判断（缺省按 ['org']，与壳层 space-visibility 同口径） */
    const supports = (item: PluginMarketItem, spaceType: 'personal' | 'org') => {
      const spaces = item.supportedSpaces && item.supportedSpaces.length > 0 ? item.supportedSpaces : ['org'];
      return spaces.includes(spaceType);
    };

    const visibleItems = computed(() => {
      const kw = keyword.value.trim().toLowerCase();
      let list = props.installedItems.filter((item) => {
        if (kw && !`${item.name} ${item.description}`.toLowerCase().includes(kw)) {
          return false;
        }
        switch (filter.value) {
          case 'updatable':
            return item.updateAvailable;
          case 'personal':
            return supports(item, 'personal');
          case 'org':
            return supports(item, 'org');
          default:
            return true;
        }
      });
      if (sort.value === 'name') {
        list = [...list].sort((a, b) => a.name.localeCompare(b.name, 'zh-Hans-CN'));
      } else if (sort.value === 'updatable') {
        list = [...list].sort((a, b) => Number(b.updateAvailable) - Number(a.updateAvailable));
      }
      return list;
    });

    const onCardDetail = (item: PluginMarketItem) => {
      emit('detail', item);
    };
    const onAddApp = () => emit('add-app');

    const handleInstallCommand = (command: string) => {
      if (command === 'market') emit('add-app');
      else if (command === 'repo') installToolsRef.value?.openRepoDialog();
      else if (command === 'sideload') installToolsRef.value?.openSideload();
    };

    return {
      keyword,
      filter,
      sort,
      installToolsRef,
      visibleItems,
      onCardDetail,
      onAddApp,
      handleInstallCommand,
      Search,
      emit
    };
  }
});
</script>
