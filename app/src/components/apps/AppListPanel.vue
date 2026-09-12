<!-- 应用清单（系统层应用管理，L4/M4 + 走查修正）：
     本页是"本机应用库存"——平铺全部已安装应用，只有 搜索 / 筛选 / 排序，
     不出现任何"当前空间"的关系提示（分组、最近使用、本空间启用标签均已移除）；
     与空间的关系只在详情页展示（已启用空间清单），启用动作在空间层的「为本空间启用」视图。
     卡片点击＝进详情；安装入口（市场 / 仓库 / 侧载）在头部下拉。 -->
<template>
  <div class="apps-list">
    <header class="apps-list-header">
      <!-- 无「应用」标题（宿主对话框/Tab 已有名）；搜索框占满剩余宽度，筛选/排序为窄下拉 -->
      <el-input
        v-model="keyword"
        class="apps-search"
        placeholder="搜索应用"
        clearable
        :prefix-icon="Search"
      />
      <!-- 筛选 / 排序（清单工具，与空间无关） -->
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
      <!-- + 安装应用 统一下拉菜单（安装＝系统层本机动作，install-and-enable §一①） -->
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
      <!-- 安装工具（隐藏按钮，通过 ref 调用其方法触发仓库/侧载安装对话框） -->
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
        <div
          v-for="item in visibleItems"
          :key="item.id"
          class="app-card"
          @click="onCardDetail(item)"
        >
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
        :description="`没有匹配的应用`"
      />
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ArrowDown, Download, MoreFilled, Search, Shop, Upload } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import AppIcon from './AppIcon.vue';
import AppInstallTools from './AppInstallTools.vue';

/** 筛选口径：all=全部；updatable=有可更新版本；personal/org=manifest 声明的适用域（supportedSpaces） */
type FilterKey = 'all' | 'updatable' | 'personal' | 'org';
type SortKey = 'default' | 'name' | 'updatable';

export default defineComponent({
  name: 'AppListPanel',
  // Options API：模板中以标签形式使用的图标组件必须注册（仅 setup return 会静默不渲染）
  components: { ArrowDown, Download, MoreFilled, Search, Shop, Upload, AppInstallTools, AppIcon },
  props: {
    /** 已安装应用（系统层全量，不按空间过滤） */
    installedItems: { type: Array as PropType<PluginMarketItemDto[]>, required: true },
    /** 各插件进行中的操作（'' | install | upgrade | toggle | uninstall），详情/卡片 busy 态用 */
    busyByPlugin: { type: Object as PropType<Record<string, string>>, default: () => ({}) }
  },
  emits: ['detail', 'add-app', 'install-repo', 'sideloaded'],
  setup(props, { emit }) {
    const keyword = ref('');
    const filter = ref<FilterKey>('all');
    const sort = ref<SortKey>('default');

    /** AppInstallTools 组件引用，用于触发仓库/侧载安装对话框 */
    const installToolsRef = ref<InstanceType<typeof AppInstallTools> | null>(null);

    /** supportedSpaces 适用域判断（缺省按 ['org']，与 space-visibility 同口径） */
    const supports = (item: PluginMarketItemDto, spaceType: 'personal' | 'org') => {
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

    /** 点卡片 / 「...」：进入应用详情（管理界面不直接启动应用，启动器是桌面九宫格/图标，D4 口径） */
    const onCardDetail = (item: PluginMarketItemDto) => {
      emit('detail', item);
    };

    /** 「添加应用」入口（空状态）：切换到应用市场 */
    const onAddApp = () => emit('add-app');

    /** 「+ 安装应用」下拉菜单命令路由 */
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
