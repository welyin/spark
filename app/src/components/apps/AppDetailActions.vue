<!-- 应用详情操作按钮组（从 AppDetailPanel 抽出）：打开/安装/启用禁用/更新/卸载。
     桌面端渲染在详情页底部操作区；移动端收进「图标+名称」卡片内（AppDetailPanel 两处引用，口径一致） -->
<template>
  <!-- 组织空间：启用/禁用（设计 §4.2）+ 管理员卸载；非管理员不暴露安装/更新/卸载 -->
  <template v-if="isOrgSpace">
    <el-button v-if="enabled" type="primary" @click="emit('open', item)">打开</el-button>
    <el-button
      v-if="!enabled"
      type="primary"
      :loading="busy === 'toggle'"
      @click="isAdmin ? emit('toggle', item) : emit('request-enable', item)"
    >
      启用
    </el-button>
    <el-button
      v-else-if="isAdmin"
      :loading="busy === 'toggle'"
      @click="emit('toggle', item)"
    >
      禁用
    </el-button>
    <el-button
      v-if="isAdmin && item.updateAvailable"
      type="warning"
      :loading="busy === 'upgrade'"
      @click="emit('upgrade', item)"
    >
      更新
    </el-button>
    <!-- 卸载仅移除插件程序，插件数据保留在本机（确认框在 AppsPage）；
         组织空间仅管理员可见，与个人空间分支同口径 -->
    <el-button
      v-if="isAdmin && item.installed"
      type="danger"
      :loading="busy === 'uninstall'"
      @click="emit('uninstall', item)"
    >
      卸载
    </el-button>
  </template>

  <!-- 个人空间：安装 / 启用 / 禁用 / 打开 / 更新（设计 §4.1/§4.3/§4.4） -->
  <template v-else>
    <el-button
      v-if="!item.installed"
      type="primary"
      :loading="busy === 'install'"
      @click="emit('install', item)"
    >
      安装
    </el-button>
    <template v-else>
      <el-button v-if="enabled" type="primary" @click="emit('open', item)">打开</el-button>
      <el-button :loading="busy === 'toggle'" @click="emit('toggle', item)">
        {{ enabled ? '禁用' : '启用' }}
      </el-button>
      <el-button
        v-if="item.updateAvailable"
        type="warning"
        :loading="busy === 'upgrade'"
        @click="emit('upgrade', item)"
      >
        更新
      </el-button>
      <!-- 卸载仅移除插件程序，插件数据保留在本机（确认框在 AppsPage） -->
      <el-button type="danger" :loading="busy === 'uninstall'" @click="emit('uninstall', item)">
        卸载
      </el-button>
    </template>
  </template>
</template>

<script lang="ts">
import { defineComponent, type PropType } from 'vue';
import type { PluginMarketItemDto } from '../../api/types';

export default defineComponent({
  name: 'AppDetailActions',
  props: {
    item: { type: Object as PropType<PluginMarketItemDto>, required: true },
    enabled: { type: Boolean, required: true },
    isOrgSpace: { type: Boolean, required: true },
    isAdmin: { type: Boolean, required: true },
    busy: { type: String, default: '' }
  },
  emits: ['open', 'install', 'upgrade', 'toggle', 'uninstall', 'request-enable'],
  setup(_, { emit }) {
    return { emit };
  }
});
</script>
