<!-- 应用详情操作按钮组：底部图标化操作栏。
     桌面端渲染在详情页底部；移动端固定在屏幕底部。
     主操作（打开/安装/启用/更新）蓝色大按钮居左，禁用/卸载等次级操作居右。 -->
<template>
  <div class="app-detail-action-bar">
    <!-- 组织空间 -->
    <template v-if="isOrgSpace">
      <!-- 主操作：打开 / 启用（含非管理员请求启用） -->
      <button
        v-if="enabled"
        type="button"
        class="action-btn action-btn--primary"
        :disabled="busy === 'open'"
        @click="emit('open', item)"
      >
        <el-icon :size="18"><VideoPlay /></el-icon>
        <span>打开</span>
      </button>
      <button
        v-else
        type="button"
        class="action-btn action-btn--primary"
        :disabled="busy === 'toggle'"
        @click="isAdmin ? emit('toggle', item) : emit('request-enable', item)"
      >
        <el-icon :size="18"><SwitchButton /></el-icon>
        <span>启用</span>
      </button>

      <!-- 次级操作：禁用（管理员且启用） -->
      <button
        v-if="isAdmin && enabled"
        type="button"
        class="action-btn action-btn--secondary"
        :disabled="busy === 'toggle'"
        @click="emit('toggle', item)"
      >
        <el-icon :size="16"><CircleClose /></el-icon>
        <span>禁用</span>
      </button>

      <!-- 更新 -->
      <button
        v-if="isAdmin && item.updateAvailable"
        type="button"
        class="action-btn action-btn--warning"
        :disabled="busy === 'upgrade'"
        @click="emit('upgrade', item)"
      >
        <el-icon :size="16"><RefreshRight /></el-icon>
        <span>更新</span>
      </button>

      <!-- 卸载（管理员） -->
      <button
        v-if="isAdmin && item.installed"
        type="button"
        class="action-btn action-btn--danger"
        :disabled="busy === 'uninstall'"
        @click="emit('uninstall', item)"
      >
        <el-icon :size="16"><Delete /></el-icon>
        <span>卸载</span>
      </button>
    </template>

    <!-- 个人空间 -->
    <template v-else>
      <!-- 未安装 -->
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

      <!-- 已安装：主操作 + 次级操作 -->
      <template v-else>
        <!-- 主操作：更新优先，其次打开，最后启用 -->
        <button
          v-if="item.updateAvailable"
          type="button"
          class="action-btn action-btn--primary"
          :disabled="busy === 'upgrade'"
          @click="emit('upgrade', item)"
        >
          <el-icon :size="18"><RefreshRight /></el-icon>
          <span>更新</span>
        </button>
        <button
          v-else-if="enabled"
          type="button"
          class="action-btn action-btn--primary"
          :disabled="busy === 'open'"
          @click="emit('open', item)"
        >
          <el-icon :size="18"><VideoPlay /></el-icon>
          <span>打开</span>
        </button>
        <button
          v-else
          type="button"
          class="action-btn action-btn--primary"
          :disabled="busy === 'toggle'"
          @click="emit('toggle', item)"
        >
          <el-icon :size="18"><SwitchButton /></el-icon>
          <span>启用</span>
        </button>

        <!-- 次级操作：启用/禁用切换始终可达（更新占主位时不丢切换入口） -->
        <button
          v-if="enabled"
          type="button"
          class="action-btn action-btn--secondary"
          :disabled="busy === 'toggle'"
          @click="emit('toggle', item)"
        >
          <el-icon :size="16"><CircleClose /></el-icon>
          <span>禁用</span>
        </button>
        <button
          v-else-if="item.updateAvailable"
          type="button"
          class="action-btn action-btn--secondary"
          :disabled="busy === 'toggle'"
          @click="emit('toggle', item)"
        >
          <el-icon :size="16"><SwitchButton /></el-icon>
          <span>启用</span>
        </button>
        <!-- 有更新时主位被「更新」占用，额外提供「打开」作为次级入口 -->
        <button
          v-if="item.updateAvailable"
          type="button"
          class="action-btn action-btn--secondary"
          :disabled="busy === 'open'"
          @click="emit('open', item)"
        >
          <el-icon :size="16"><VideoPlay /></el-icon>
          <span>打开</span>
        </button>

        <!-- 卸载 -->
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
    </template>
  </div>
</template>

<script lang="ts">
import { defineComponent, type PropType } from 'vue';
import { CircleClose, Delete, Download, RefreshRight, SwitchButton, VideoPlay } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';

export default defineComponent({
  name: 'AppDetailActions',
  components: { CircleClose, Delete, Download, RefreshRight, SwitchButton, VideoPlay },
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
