<!-- 设备管理模块（MinePage「设备管理」第三、四栏；设置页以抽屉复用）：
     展示用该账号登录过的设备及其在线状态。
     TODO(mock): 多设备登录记录与在线状态依赖后续内核接口（当前为纯本地 P2P，无设备清单），
     本期为 UI 占位，仅展示本机设备（在线） -->
<template>
  <!-- 第三栏：设备列表 -->
  <div class="mine-list">
    <h2 class="mine-list-title">设备管理</h2>
    <div class="mine-list-items">
      <button
        type="button"
        class="mine-list-item"
        :class="{ active: activeItem === 'device' }"
        @click="activeItem = 'device'"
      >
        <!-- 移动端菜单图标补色（微信式每项一色，同 MinePage 一级菜单色板；桌面端不生效） -->
        <el-icon class="mine-list-item-icon" :size="17" :style="isMobileLayout ? { color: '#3296fa' } : undefined"><Monitor /></el-icon>
        <span class="mine-list-item-text">
          <b>本机设备</b>
          <span>{{ deviceSummary }}</span>
        </span>
        <el-tag type="success" size="small">在线</el-tag>
      </button>
    </div>
  </div>

  <!-- 详情：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeItem !== null"
    title="本机设备"
    @close="activeItem = null"
  >
    <el-card shadow="never" class="panel-card">
      <template #header>
        <h2>本机设备</h2>
      </template>
      <div class="device-status">
        <el-tag type="success">在线</el-tag>
        <span class="device-status-text">这是当前正在使用的设备</span>
      </div>
      <NodeIdentityInfo class="device-rows" :rows="deviceRows" />
      <p class="hint">其他设备的登录记录与在线状态将在后续版本提供，当前仅展示本机设备。</p>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { Monitor } from '@element-plus/icons-vue';
import NodeIdentityInfo, { type NodeIdentityRow } from '../common/NodeIdentityInfo.vue';
import { isMobileLayout } from '../../stores/ui-layout';
import type { P2pInfoDto as P2PInfo } from '../../api';
import MineDetailContainer from './MineDetailContainer.vue';

export default defineComponent({
  name: 'DevicesModule',
  components: { NodeIdentityInfo, MineDetailContainer, Monitor },
  props: {
    rootId: { type: String, default: '' },
    p2pInfo: { type: Object as PropType<P2PInfo>, required: true },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持默认选中本机设备
    const activeItem = ref<'device' | null>(props.detailMode === 'drawer' ? null : 'device');

    const deviceSummary = computed(() =>
      props.p2pInfo.started && props.p2pInfo.peerId ? '节点在线' : '节点未启动'
    );

    const deviceRows = computed<NodeIdentityRow[]>(() => [
      { label: '身份 ID', term: 'rootId', value: props.rootId, copyable: true, emptyText: '未创建' },
      { label: '设备标识', term: 'peerId', value: props.p2pInfo.peerId ?? '', emptyText: '节点未启动' },
      { label: '节点地址', term: 'addresses', value: props.p2pInfo.addresses, emptyText: '节点未启动' }
    ]);

    return {
      activeItem,
      deviceSummary,
      deviceRows,
      isMobileLayout
    };
  }
});
</script>

<style scoped>
.device-status {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
}

.device-status-text {
  font-size: 13px;
  color: var(--spark-text-2);
}

.device-rows {
  margin-bottom: 16px;
}
</style>
