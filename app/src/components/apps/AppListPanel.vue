<template>
  <div class="apps-list">
    <header class="apps-list-header">
      <h1 class="apps-title">应用</h1>
      <el-input
        v-model="keyword"
        class="apps-search"
        placeholder="搜索应用"
        clearable
        :prefix-icon="Search"
      />
      <el-button type="primary" :icon="Plus" @click="onAddGroup">添加分类</el-button>
    </header>

    <el-empty v-if="installedItems.length === 0" description="还没有安装应用，去市场看看吧">
      <el-button type="primary" @click="onAddApp">进入应用市场</el-button>
    </el-empty>

    <template v-else>
      <!-- 「添加应用」虚线入口卡片：恒在第一行，点击进入应用市场 -->
      <div class="app-cards">
        <button type="button" class="app-add-card" @click="onAddApp">
          <el-icon :size="26"><Plus /></el-icon>
          <span>添加应用</span>
        </button>
      </div>

      <!-- 「最近使用」为系统自动分组，不可编辑、不可作为拖放目标（ui-apps-market §2.3） -->
      <section v-if="visibleRecent.length > 0" class="app-group">
        <header class="app-group-header" @click="toggleCollapse('__recent__')">
          <el-icon class="app-group-arrow" :class="{ collapsed: isCollapsed('__recent__') }"><ArrowRight /></el-icon>
          <span class="app-group-name">最近使用</span>
          <span class="app-group-count">{{ visibleRecent.length }}</span>
        </header>
        <div v-show="!isCollapsed('__recent__')" class="app-cards">
          <div
            v-for="item in visibleRecent"
            :key="`recent-${item.id}`"
            class="app-card"
            :class="{ disabled: !isEnabled(item) || isSuspended(item), 'drag-source': hiddenAppId === item.id }"
            draggable="true"
            @dragstart="onDragStart($event, item.id)"
            @dragend="onDragEnd"
          >
            <span class="app-card-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
            <div class="app-card-body" @click="onCardClick(item)">
              <div class="app-card-head">
                <span class="app-card-name">{{ item.name }}</span>
                <el-tag v-if="isSuspended(item)" size="small" type="warning">已停用</el-tag>
                <el-tag size="small" effect="plain" :type="isEnabled(item) ? 'success' : 'info'">
                  {{ isEnabled(item) ? '启用' : '禁用' }}
                </el-tag>
                <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
              </div>
              <p class="app-card-desc">{{ item.description }}</p>
            </div>
            <div class="app-card-actions">
              <el-button v-if="isEnabled(item)" size="small" type="primary" @click="emit('open', item)">打开</el-button>
              <el-button size="small" @click="emit('toggle', item)">{{ isEnabled(item) ? '禁用' : '启用' }}</el-button>
              <!-- 卸载仅移除插件程序，插件数据保留在本机（确认框在 AppsPage） -->
              <el-button size="small" type="danger" plain @click="emit('uninstall', item)">卸载</el-button>
            </div>
          </div>
        </div>
      </section>

      <!-- 分类分组：组头可折叠/重命名/删除；整个分区（组头+卡片区）都是拖放目标，
           拖到卡片左/右半边=插入到该卡前/后，拖到空白=移到该分类末尾 -->
      <section
        v-for="section in visibleGroups"
        :key="section.name"
        class="app-group"
        :class="{ 'drop-target': dropTarget?.group === section.name }"
        @dragenter.prevent
        @dragover="onSectionDragOver($event, section)"
        @drop.prevent="onDropSection(section)"
      >
        <header class="app-group-header" @click="toggleCollapse(section.name)">
          <el-icon class="app-group-arrow" :class="{ collapsed: isCollapsed(section.name) }"><ArrowRight /></el-icon>
          <span class="app-group-name">{{ section.name }}</span>
          <span class="app-group-count">{{ section.items.length }}</span>
          <span class="app-group-actions">
            <button type="button" class="app-group-action" title="重命名分类" @click.stop="onRenameGroup(section.name)">
              <el-icon :size="13"><Edit /></el-icon>
            </button>
            <button type="button" class="app-group-action" title="删除分类" @click.stop="onDeleteGroup(section.name)">
              <el-icon :size="13"><Delete /></el-icon>
            </button>
          </span>
        </header>
        <div v-show="!isCollapsed(section.name)" class="app-cards">
          <template v-for="(item, index) in section.items" :key="item.id">
            <!-- 落点预测：拖拽悬停时在插入位渲染占位块（与真实卡片同格、虚线框明显区分） -->
            <div v-if="isDropBefore(section.name, index)" class="app-card-drop-placeholder" />
            <div
              class="app-card"
              :class="{ disabled: !isEnabled(item) || isSuspended(item), 'drag-source': hiddenAppId === item.id }"
              draggable="true"
              @dragstart="onDragStart($event, item.id)"
              @dragenter.prevent
              @dragover="onCardDragOver($event, section, index)"
              @dragend="onDragEnd"
            >
            <span class="app-card-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
            <div class="app-card-body" @click="onCardClick(item)">
              <div class="app-card-head">
                <span class="app-card-name">{{ item.name }}</span>
                <el-tag v-if="isSuspended(item)" size="small" type="warning">已停用</el-tag>
                <el-tag size="small" effect="plain" :type="isEnabled(item) ? 'success' : 'info'">
                  {{ isEnabled(item) ? '启用' : '禁用' }}
                </el-tag>
                <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
              </div>
              <p class="app-card-desc">{{ item.description }}</p>
            </div>
            <div class="app-card-actions">
              <el-button v-if="isEnabled(item)" size="small" type="primary" @click="emit('open', item)">打开</el-button>
              <el-button size="small" @click="emit('toggle', item)">{{ isEnabled(item) ? '禁用' : '启用' }}</el-button>
              <!-- 卸载仅移除插件程序，插件数据保留在本机（确认框在 AppsPage） -->
              <el-button size="small" type="danger" plain @click="emit('uninstall', item)">卸载</el-button>
            </div>
            </div>
          </template>
          <!-- 落点预测：插入位在末尾时占位块追加在最后 -->
          <div v-if="isDropBefore(section.name, section.items.length)" class="app-card-drop-placeholder" />
        </div>
      </section>

      <el-empty
        v-if="keyword.trim() && visibleGroups.length === 0 && visibleRecent.length === 0"
        :description="`没有匹配「${keyword.trim()}」的应用`"
      />
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { ArrowRight, Delete, Edit, Plus, Search } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { appIconBackground, type useAppGroups } from './apps-store';

export default defineComponent({
  name: 'AppListPanel',
  // Options API：模板中以标签形式使用的图标组件必须注册（仅 setup return 会静默不渲染）
  components: { ArrowRight, Plus, Edit, Delete },
  props: {
    /** 已安装应用（含已禁用，禁用卡片置灰展示，ui-apps-market §2.5） */
    installedItems: { type: Array as PropType<PluginMarketItemDto[]>, required: true },
    recentItems: { type: Array as PropType<PluginMarketItemDto[]>, required: true },
    isEnabled: { type: Function as PropType<(item: PluginMarketItemDto) => boolean>, required: true },
    /** 熔断自动停用判定（崩溃环）：命中卡片置灰 + 「已停用」徽标 */
    isSuspended: { type: Function as PropType<(item: PluginMarketItemDto) => boolean>, required: true },
    groups: { type: Object as PropType<ReturnType<typeof useAppGroups>>, required: true }
  },
  emits: ['open', 'detail', 'toggle', 'uninstall', 'add-app'],
  setup(props, { emit }) {
    const keyword = ref('');
    const collapsed = ref<Record<string, boolean>>({});
    /** 拖拽移动/排序应用：当前拖拽中的应用 id 与插入目标（分类名 + 插入位） */
    const dragAppId = ref('');
    /** 延迟隐藏的源卡片 id：dragstart 同步隐藏源元素会被 WebKit 取消拖拽会话，等一帧再隐藏 */
    const hiddenAppId = ref('');
    const dropTarget = ref<{ group: string; index: number } | null>(null);

    const matchesKeyword = (item: PluginMarketItemDto) =>
      item.name.toLowerCase().includes(keyword.value.trim().toLowerCase());

    /** 组内排序：有排序值的按序号排，无排序值的按原始顺序排在后面 */
    const sortByOrder = (items: PluginMarketItemDto[]) =>
      items
        .map((item, index) => ({ item, index, order: props.groups.orderOf(item.id) }))
        .sort((a, b) => (a.order ?? Number.MAX_SAFE_INTEGER) - (b.order ?? Number.MAX_SAFE_INTEGER) || a.index - b.index)
        .map(({ item }) => item);

    const visibleRecent = computed(() => props.recentItems.filter(matchesKeyword));

    // 非搜索态下空分组也展示（新建分类可见、可作为拖放目标）；搜索态只显示有命中的分组
    const visibleGroups = computed(() =>
      props.groups.allGroups.value
        .map((name) => ({
          name,
          items: sortByOrder(
            props.installedItems.filter((item) => props.groups.groupOf(item.id) === name && matchesKeyword(item))
          )
        }))
        .filter((section) => (keyword.value.trim() ? section.items.length > 0 : true))
    );

    const isCollapsed = (name: string) => collapsed.value[name] ?? false;
    const toggleCollapse = (name: string) => {
      collapsed.value = { ...collapsed.value, [name]: !isCollapsed(name) };
    };

    /** 点卡片主体：启用中进入详情（打开走「打开」按钮）；已禁用进详情便于重新启用（§2.5） */
    const onCardClick = (item: PluginMarketItemDto) => {
      emit('detail', item);
    };

    /** 「添加应用」入口（空状态 / 虚线卡片共用）：切换到应用市场 */
    const onAddApp = () => emit('add-app');

    // ---- 分类增删改 ----

    const onAddGroup = async () => {
      let name = '';
      try {
        const result = await ElMessageBox.prompt('请输入分类名称', '添加分类', {
          confirmButtonText: '创建',
          cancelButtonText: '取消',
          inputPlaceholder: '例如：效率'
        });
        name = result.value ?? '';
      } catch {
        return; // 用户取消
      }
      if (!props.groups.createGroup(name)) {
        ElMessage.warning('分类名称为空或已存在');
      }
    };

    const onRenameGroup = async (name: string) => {
      let newName = '';
      try {
        const result = await ElMessageBox.prompt('请输入新的分类名称', '重命名分类', {
          confirmButtonText: '保存',
          cancelButtonText: '取消',
          inputValue: name
        });
        newName = result.value ?? '';
      } catch {
        return;
      }
      if (!props.groups.renameGroup(name, newName)) {
        ElMessage.warning('分类名称为空或已存在');
      }
    };

    const onDeleteGroup = async (name: string) => {
      try {
        await ElMessageBox.confirm(`确定删除分类「${name}」吗？组内应用将移到回退分类。`, '删除分类', {
          confirmButtonText: '删除',
          cancelButtonText: '取消',
          type: 'warning'
        });
      } catch {
        return;
      }
      if (!props.groups.deleteGroup(name)) {
        ElMessage.warning('至少保留一个分类');
      }
    };

    // ---- 拖拽：整个分区都是拖放目标；卡片左/右半边=插入到该卡前/后，空白=分类末尾 ----

    const onDragStart = (event: DragEvent, pluginId: string) => {
      dragAppId.value = pluginId;
      // WebKit 兼容：dragstart 必须写入 dataTransfer，拖拽会话才算有效
      event.dataTransfer?.setData('text/plain', pluginId);
      if (event.dataTransfer) {
        event.dataTransfer.effectAllowed = 'move';
      }
      // 延迟隐藏源卡片（同步隐藏会被 WebKit 取消拖拽会话）
      setTimeout(() => {
        hiddenAppId.value = dragAppId.value;
      }, 0);
    };

    const onDragEnd = () => {
      dragAppId.value = '';
      hiddenAppId.value = '';
      dropTarget.value = null;
    };

    /** 分区空白处（含组头）：插入位=分类末尾；具体位置由卡片级 dragover 细化 */
    const onSectionDragOver = (event: DragEvent, section: { name: string; items: PluginMarketItemDto[] }) => {
      if (!dragAppId.value) {
        return;
      }
      event.preventDefault();
      if (dropTarget.value?.group !== section.name) {
        dropTarget.value = { group: section.name, index: section.items.length };
      }
    };

    /** 卡片上：左半边=插到该卡前，右半边=插到该卡后（横向网格布局按 X 判断） */
    const onCardDragOver = (event: DragEvent, section: { name: string }, index: number) => {
      if (!dragAppId.value) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      const before = event.clientX < rect.left + rect.width / 2;
      dropTarget.value = { group: section.name, index: before ? index : index + 1 };
    };

    /** 插入位高亮：落在某卡片索引处时给该卡上插入指示线 */
    const isDropBefore = (group: string, index: number) =>
      dragAppId.value !== '' && dropTarget.value?.group === group && dropTarget.value.index === index;

    const onDropSection = (section: { name: string; items: PluginMarketItemDto[] }) => {
      const appId = dragAppId.value;
      const target = dropTarget.value;
      if (appId && target) {
        // 在当前顺序（含排序值）上重排：先摘除拖拽项（同组时目标位前移），再插到目标位
        const ids = section.items.map((item) => item.id);
        const fromIndex = ids.indexOf(appId);
        let toIndex = target.index;
        if (fromIndex !== -1) {
          ids.splice(fromIndex, 1);
          if (fromIndex < toIndex) {
            toIndex -= 1;
          }
        }
        ids.splice(Math.max(0, Math.min(toIndex, ids.length)), 0, appId);
        if (props.groups.groupOf(appId) !== section.name) {
          props.groups.moveToGroup(appId, section.name);
        }
        props.groups.persistOrder(ids);
      }
      onDragEnd();
    };

    return {
      keyword,
      hiddenAppId,
      visibleRecent,
      visibleGroups,
      dropTarget,
      isCollapsed,
      toggleCollapse,
      onCardClick,
      onAddApp,
      onAddGroup,
      onRenameGroup,
      onDeleteGroup,
      onDragStart,
      onDragEnd,
      onSectionDragOver,
      onCardDragOver,
      isDropBefore,
      onDropSection,
      appIconBackground,
      Plus,
      Search,
      emit
    };
  }
});
</script>
