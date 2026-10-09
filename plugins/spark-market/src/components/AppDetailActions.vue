<!-- 应用详情操作按钮组（系统层应用管理，移植自壳层 AppDetailActions）：
     只承载系统层本机动作——安装 / 更新 / 卸载。「打开」与「启用/停用」是
     空间层动作（壳层空间桌面/空间市场），本插件不提供。
     插件版不含 dev 插件判定（dev 链路条目不进 sdk.market 数据源）。 -->
<template>
  <div class="app-detail-action-bar">
    <!-- 未安装：安装（获取代码＝系统层本机动作，install-and-enable §一①） -->
    <button
      v-if="!item.installed"
      type="button"
      class="action-btn action-btn--primary action-btn--full"
      :disabled="busy === 'install'"
      @click="emit('install', item)"
    >
      <el-icon :size="18"><Download /></el-icon>
      <span>安装</span>
    </button>

    <template v-else>
      <!-- 更新（系统层本机动作） -->
      <button
        v-if="item.updateAvailable"
        type="button"
        class="action-btn action-btn--warning"
        :disabled="busy === 'upgrade'"
        @click="emit('upgrade', item)"
      >
        <el-icon :size="16"><RefreshRight /></el-icon>
        <span>更新</span>
      </button>

      <!-- 卸载（系统层本机动作：仅移除本机程序、不删数据） -->
      <button
        type="button"
        class="action-btn action-btn--danger"
        :disabled="busy === 'uninstall'"
        @click="emit('uninstall', item)"
      >
        <el-icon :size="16"><Delete /></el-icon>
        <span>卸载</span>
      </button>
    </template>
  </div>
</template>

<script lang="ts">
import { defineComponent, type PropType } from 'vue';
import { Delete, Download, RefreshRight } from '@element-plus/icons-vue';
import type { PluginMarketItem } from '../../../../packages/plugin-sdk/src';

export default defineComponent({
  name: 'AppDetailActions',
  components: { Delete, Download, RefreshRight },
  props: {
    item: { type: Object as PropType<PluginMarketItem>, required: true },
    busy: { type: String, default: '' }
  },
  emits: ['install', 'upgrade', 'uninstall'],
  setup(_, { emit }) {
    return { emit };
  }
});
</script>
