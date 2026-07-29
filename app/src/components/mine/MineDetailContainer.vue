<!-- mine 模块详情容器：column 模式渲染原第四栏 .mine-detail div；drawer 模式渲染 el-drawer。
     各模块的详情/编辑内容作为 slot 传入，同一份内容两种容器复用（设置页「个人设置」用抽屉，
     MinePage 个人设置页保持第四栏）。
     抽屉样式全 app 统一（app-shell.css .app-drawer）：无默认头部小标题、内容卡片去边框，
     卡片标题即抽屉标题（顶在左上角），关闭按钮为右上角自定义 X -->
<template>
  <el-drawer
    v-if="drawer"
    :model-value="open"
    :with-header="false"
    size="420px"
    class="app-drawer"
    @update:model-value="onUpdate"
  >
    <button type="button" class="app-drawer-close" title="关闭" @click="emit('close')">
      <el-icon :size="16"><Close /></el-icon>
    </button>
    <!-- 复用 .mine-detail：卡片去边框/阴影（mine.css），内容铺满抽屉 -->
    <div class="mine-detail app-drawer-fill">
      <slot />
    </div>
  </el-drawer>
  <div v-else class="mine-detail">
    <slot />
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { Close } from '@element-plus/icons-vue';

export default defineComponent({
  name: 'MineDetailContainer',
  components: { Close },
  props: {
    /** true=抽屉模式（设置页）；false=第四栏模式（个人设置页） */
    drawer: { type: Boolean, default: false },
    /** 抽屉是否打开（通常绑定「是否有选中项」） */
    open: { type: Boolean, default: false },
    /** 保留 prop（各模块仍传入）：抽屉已不再显示头部标题，卡片标题即标题 */
    title: { type: String, default: '' }
  },
  emits: ['close'],
  setup(_, { emit }) {
    const onUpdate = (value: boolean) => {
      if (!value) {
        emit('close');
      }
    };
    return { onUpdate, emit };
  }
});
</script>
