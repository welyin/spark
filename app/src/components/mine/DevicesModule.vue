<!-- 设备管理模块（MinePage「设备管理」第三、四栏；设置页以抽屉复用）：
     展示同一身份下的全部设备（本机 + 已配对自设备）及其在线状态。
     数据来自内核设备清单（devices.list）：本机条目由 p2p 启动时采集落库
     （设备名/操作系统/架构/物理地址），其他设备条目经 device-sync 自设备
     通道同步；DeviceUpdated 事件触发刷新。 -->
<template>
  <!-- 第三栏：设备列表 -->
  <div class="mine-list">
    <h2 class="mine-list-title">设备管理</h2>
    <div class="mine-list-items">
      <button
        v-for="device in devices"
        :key="device.peerId"
        type="button"
        class="mine-list-item"
        :class="{ active: activePeerId === device.peerId }"
        @click="activePeerId = device.peerId"
      >
        <el-icon class="mine-list-item-icon" :size="17" :style="{ color: '#3296fa' }"><Monitor /></el-icon>
        <span class="mine-list-item-text">
          <b>{{ device.isSelf ? '本机设备' : device.deviceName }}</b>
          <span>{{ deviceSummary(device) }}</span>
        </span>
        <el-tag :type="device.online ? 'success' : 'info'" size="small">
          {{ device.online ? '在线' : '离线' }}
        </el-tag>
      </button>
      <p v-if="!devices.length" class="devices-empty">暂无设备记录</p>
    </div>
  </div>

  <!-- 详情：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeDevice !== null"
    :title="activeDevice?.isSelf ? '本机设备' : (activeDevice?.deviceName ?? '设备详情')"
    @close="activePeerId = null"
  >
    <el-card v-if="activeDevice" shadow="never" class="panel-card">
      <template #header>
        <h2>{{ activeDevice.isSelf ? '本机设备' : activeDevice.deviceName }}</h2>
      </template>
      <div class="device-status">
        <el-tag :type="activeDevice.online ? 'success' : 'info'">
          {{ activeDevice.online ? '在线' : '离线' }}
        </el-tag>
        <span class="device-status-text">
          {{ activeDevice.isSelf ? '这是当前正在使用的设备' : '同一账号登录的设备' }}
        </span>
      </div>
      <div class="device-rows">
        <div class="device-row">
          <span class="device-row-label">设备名</span>
          <span class="device-row-value">{{ activeDevice.deviceName }}</span>
        </div>
        <div class="device-row">
          <span class="device-row-label">操作系统</span>
          <span class="device-row-value">{{ activeDevice.os }}（{{ activeDevice.arch }}）</span>
        </div>
        <div v-if="activeDevice.macs.length" class="device-row">
          <span class="device-row-label">物理地址</span>
          <span class="device-row-value">{{ activeDevice.macs.join('、') }}</span>
        </div>
        <div class="device-row">
          <span class="device-row-label">设备标识</span>
          <span class="device-row-value device-row-mono">{{ shortPeerId(activeDevice.peerId) }}</span>
        </div>
        <div v-if="!activeDevice.isSelf" class="device-row">
          <span class="device-row-label">最近同步</span>
          <span class="device-row-value">{{ formatTime(activeDevice.lastSeenAt) }}</span>
        </div>
      </div>
      <p class="hint">设备信息经端到端签名通道在同账号设备间自动同步。</p>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, type PropType } from 'vue';
import { Monitor } from '@element-plus/icons-vue';
import { listenP2pEvents, type DeviceDto, type P2pInfoDto as P2PInfo } from '../../api';
import type { UnlistenFn } from '@tauri-apps/api/event';
import MineDetailContainer from './MineDetailContainer.vue';

export default defineComponent({
  name: 'DevicesModule',
  components: { MineDetailContainer, Monitor },
  props: {
    rootId: { type: String, default: '' },
    p2pInfo: { type: Object as PropType<P2PInfo>, required: true },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    const devices = ref<DeviceDto[]>([]);
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式默认选中本机设备
    const activePeerId = ref<string | null>(null);

    const load = async () => {
      try {
        devices.value = await window.electronAPI.devices.list();
        // 默认选中：column 模式选中本机；已选中设备仍在清单则保持
        if (activePeerId.value && !devices.value.some((d) => d.peerId === activePeerId.value)) {
          activePeerId.value = null;
        }
        if (!activePeerId.value && props.detailMode === 'column') {
          activePeerId.value = devices.value.find((d) => d.isSelf)?.peerId ?? null;
        }
      } catch (e) {
        console.warn('[DevicesModule] 加载设备清单失败', e);
      }
    };

    let unlisten: UnlistenFn | undefined;
    onMounted(async () => {
      await load();
      // device-sync 落库 / 本机采集刷新 → 清单刷新（非 Tauri 环境订阅失败静默）
      try {
        unlisten = await listenP2pEvents((event) => {
          if (event.kind === 'DeviceUpdated') {
            void load();
          }
        });
      } catch {
        // 单测/mock 环境无事件桥
      }
    });
    onBeforeUnmount(() => unlisten?.());

    const activeDevice = computed(
      () => devices.value.find((d) => d.peerId === activePeerId.value) ?? null
    );

    const deviceSummary = (device: DeviceDto) =>
      device.isSelf ? `${device.deviceName} · ${device.os}` : `${device.os} · ${device.arch}`;

    /** peerId 长串截断展示（前 8…后 6） */
    const shortPeerId = (peerId: string) =>
      peerId.length > 20 ? `${peerId.slice(0, 8)}…${peerId.slice(-6)}` : peerId;

    const formatTime = (ts: number) => {
      if (!ts) {
        return '—';
      }
      const d = new Date(ts);
      const pad = (n: number) => String(n).padStart(2, '0');
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
    };

    return {
      devices,
      activePeerId,
      activeDevice,
      deviceSummary,
      shortPeerId,
      formatTime
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
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.device-row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  font-size: 13px;
}

.device-row-label {
  flex: 0 0 64px;
  color: var(--spark-text-2);
}

.device-row-value {
  flex: 1;
  min-width: 0;
  word-break: break-all;
  color: var(--spark-text-1, inherit);
}

.device-row-mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}

.devices-empty {
  padding: 12px;
  font-size: 13px;
  color: var(--spark-text-2);
}
</style>
