<!-- 空间的应用市场（空间层，install-and-enable §一形式化定义）：
     目录版式与系统层应用管理的市场一致（AppMarketPanel mode="space"），动作只有
     启用/停用——在应用详情页操作（AppDetailPanel mode="space"），装卸在系统层应用管理。
     只列当前空间适用的应用，头部有「只看已启用」筛选；启用＝逻辑状态，未安装的应用
     首次打开时由插件宿主提示就地安装。
     承载：PC＝空间桌面窗口（spark:space-market）；移动端＝应用 Tab 内整页视图（返回栏在外层）。 -->
<template>
  <div class="space-market-view">
    <AppDetailPanel
      v-if="detailItem"
      mode="space"
      :item="detailItem"
      @back="detailItem = null"
    />
    <AppMarketPanel v-else mode="space" :items="items" @detail="onDetail" />
  </div>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import type { PluginMarketItemDto } from '../../api/types';
import AppMarketPanel from './AppMarketPanel.vue';
import AppDetailPanel from './AppDetailPanel.vue';
import { listMockApps } from '../../mock/apps';
import { listDevPlugins } from '../../mock/dev-plugins';
import { mockMode } from '../../mock/mode';

export default defineComponent({
  name: 'SpaceMarketView',
  components: { AppMarketPanel, AppDetailPanel },
  setup() {
    /** 市场条目（自加载；启用切换后重拉以刷新安装态标记） */
    const items = ref<PluginMarketItemDto[]>([]);
    const load = async () => {
      try {
        const real = await window.electronAPI.pluginMarket.list();
        // 按 id 去重：内核条目优先于 dev 注入条目（同插件不重复出现）
        const merged = new Map<string, PluginMarketItemDto>();
        for (const item of listDevPlugins()) {
          merged.set(item.id, item);
        }
        if (mockMode()) {
          for (const item of listMockApps()) {
            merged.set(item.id, item);
          }
        }
        for (const item of real) {
          merged.set(item.id, item);
        }
        items.value = [...merged.values()];
        // 详情打开中的条目同步换新（安装态等）
        if (detailItem.value) {
          detailItem.value = merged.get(detailItem.value.id) ?? detailItem.value;
        }
      } catch {
        // 读取失败保留当前清单
      }
    };
    onMounted(load);

    const detailItem = ref<PluginMarketItemDto | null>(null);
    const onDetail = (item: PluginMarketItemDto) => {
      detailItem.value = item;
    };

    return { items, detailItem, onDetail };
  }
});
</script>

<style scoped>
.space-market-view {
  height: 100%;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
}
</style>
