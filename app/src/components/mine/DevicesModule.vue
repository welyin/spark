<!-- 设备管理模块（MinePage「设备管理」第三、四栏；设置页以抽屉复用）：
     展示同一身份下的全部设备（本机 + 已配对自设备）及其在线状态。
     数据来自内核设备清单（devices.list）：本机条目由 p2p 启动时采集落库
     （设备名/操作系统/架构/物理地址），其他设备条目经 device-sync 自设备
     通道同步；DeviceUpdated 事件触发刷新。
     M1：新设备加入的通知红点在入口（MinePage/SettingsPage），本页挂载即标记已读，
     当次查看中新设备行带「新加入」标签（stores/device-notices）。
     M2：非本机且未撤销行提供「撤销」（m1-m2-implementation-plan §4.3）；已撤销行
     灰化 + 「已撤销」标签，无操作。 -->
<template>
  <!-- 第三栏：设备列表 -->
  <div class="mine-list">
    <h2 class="mine-list-title">设备管理</h2>
    <div class="mine-list-items">
      <!-- 行容器用 div 不用 button：行内嵌「撤销」按钮，HTML 不允许 button 套 button；
           视觉由 .mine-list-item 类保证与既有列表行一致（mine.css 为纯类选择器）；
           role/tabindex/keydown 补回键盘可达性（Enter/Space 选中，Space prevent 防滚动） -->
      <div
        v-for="device in devices"
        :key="device.peerId"
        class="mine-list-item"
        :class="{ active: activePeerId === device.peerId, 'device-revoked': isRevoked(device) }"
        role="button"
        tabindex="0"
        @click="activePeerId = device.peerId"
        @keydown.enter="activePeerId = device.peerId"
        @keydown.space.prevent="activePeerId = device.peerId"
      >
        <el-icon class="mine-list-item-icon" :size="17" :style="{ color: isRevoked(device) ? 'var(--spark-text-3)' : '#3296fa' }"><Monitor /></el-icon>
        <span class="mine-list-item-text">
          <b>{{ device.isSelf ? '本机设备' : device.deviceName }}</b>
          <span>{{ deviceSummary(device) }}</span>
        </span>
        <!-- 已撤销行：仅「已撤销」标签（不再显示在线/可更新/新加入），无操作 -->
        <el-tag v-if="isRevoked(device)" type="info" size="small">已撤销</el-tag>
        <template v-else>
          <el-tag v-if="newJoinedPeerIds.includes(device.peerId)" type="warning" size="small">新加入</el-tag>
          <el-tag :type="device.online ? 'success' : 'info'" size="small">
            {{ device.online ? '在线' : '离线' }}
          </el-tag>
          <el-tag v-if="hasUpdate(device)" type="warning" size="small">可更新</el-tag>
          <!-- M2 撤销：仅非本机行显示（本机不可用走「锁定设备」）；stop 不触发行选中 -->
          <el-button
            v-if="canRevoke(device)"
            text
            type="danger"
            size="small"
            class="device-revoke-btn"
            @click.stop="confirmRevoke(device)"
          >
            撤销
          </el-button>
        </template>
      </div>
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
        <el-tag v-if="isRevoked(activeDevice)" type="info">已撤销</el-tag>
        <el-tag v-else :type="activeDevice.online ? 'success' : 'info'">
          {{ activeDevice.online ? '在线' : '离线' }}
        </el-tag>
        <span class="device-status-text">
          {{ isRevoked(activeDevice) ? '该设备已被撤销，无法连接本账号' : activeDevice.isSelf ? '这是当前正在使用的设备' : '同一账号登录的设备' }}
        </span>
      </div>
      <div class="device-rows">
        <div class="device-row">
          <span class="device-row-label">设备名</span>
          <span class="device-row-value">{{ activeDevice.deviceName }}</span>
        </div>
        <div class="device-row">
          <span class="device-row-label">操作系统</span>
          <span class="device-row-value">{{ osLine(activeDevice) }}</span>
        </div>
        <div class="device-row">
          <span class="device-row-label">软件版本</span>
          <span class="device-row-value">
            {{ versionText(activeDevice) }}
            <el-tag v-if="hasUpdate(activeDevice) && !isRevoked(activeDevice)" type="warning" size="small">可更新</el-tag>
          </span>
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
        <div v-if="isRevoked(activeDevice)" class="device-row">
          <span class="device-row-label">撤销时间</span>
          <span class="device-row-value">{{ formatTime(activeDevice.revokedAt ?? 0) }}</span>
        </div>
      </div>
      <p class="hint">设备信息经端到端签名通道在同账号设备间自动同步。</p>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Monitor } from '@element-plus/icons-vue';
import { listenP2pEvents, type DeviceDto, type P2pInfoDto as P2PInfo } from '../../api';
import { compareVersions } from '../../utils/version';
import { currentSpace } from '../../stores/current-space';
import { spaceKeyOf } from '../../mock/space-key';
import { notifyDeviceRevoked } from '../../plugin/messages';
import {
  markDeviceNoticesSeen,
  pendingDeviceNotices,
  setCurrentDevicePeerId
} from '../../stores/device-notices';
import type { UnlistenFn } from '@tauri-apps/api/event';
import MineDetailContainer from './MineDetailContainer.vue';

export default defineComponent({
  name: 'DevicesModule',
  components: { MineDetailContainer, Monitor },
  props: {
    rootId: { type: String, default: '' },
    // p2pInfo 当前仅作占位（设备在线状态来自内核设备清单），保留可选以免调用方强依赖
    p2pInfo: { type: Object as PropType<P2PInfo>, default: null },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    const devices = ref<DeviceDto[]>([]);
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式默认选中本机设备
    const activePeerId = ref<string | null>(null);
    // updater 最近一次检查到的可用版本（仅存在更新时非空），供「可更新」提示对比
    const availableVersion = ref<string | null>(null);
    // M1 本次查看的「新加入」设备快照：挂载时取 pending 通知后随即标记已读清红点，
    // 标签仅保留到当次查看（下次进入不再有）
    const newJoinedPeerIds = ref<string[]>([]);

    const load = async () => {
      try {
        devices.value = await window.electronAPI.devices.list();
        // M1 本机 peerId 回写：本机的加入通知到达本机时直接忽略（stores/device-notices）
        setCurrentDevicePeerId(devices.value.find((d) => d.isSelf)?.peerId ?? null);
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
    /** 拉取 updater 最近一次检查结果：availableVersion 仅在有更新时存在（commands/updater.rs） */
    const loadUpdater = async () => {
      try {
        const status = await window.electronAPI?.updater?.status?.();
        availableVersion.value = status?.lastCheck?.availableVersion ?? null;
      } catch {
        // 无更新源/检查失败：不显示「可更新」提示
      }
    };

    onMounted(async () => {
      // M1：先捕获本次查看的「新加入」快照，再标记已读清入口红点（顺序不可换）
      newJoinedPeerIds.value = pendingDeviceNotices.value.map((notice) => notice.deviceId);
      markDeviceNoticesSeen(props.rootId);
      await load();
      await loadUpdater();
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

    /** 操作系统行：有 OS 版本则并入（如 Windows（10.0.22631 · x86_64）） */
    const osLine = (device: DeviceDto) =>
      device.osVersion
        ? `${device.os}（${device.osVersion} · ${device.arch}）`
        : `${device.os}（${device.arch}）`;

    /** 软件版本行：空值（旧记录/旧版本对端）展示「—」 */
    const versionText = (device: DeviceDto) => (device.appVersion ? `v${device.appVersion}` : '—');

    /** 该设备是否有可用更新：仅本地提示（不控制对端），与 updater 最近一次检查结果对比 */
    const hasUpdate = (device: DeviceDto) =>
      Boolean(device.appVersion) &&
      availableVersion.value !== null &&
      compareVersions(device.appVersion, availableVersion.value) < 0;

    /** M2 已撤销判定：记录保留作黑名单与灰态数据源（revokedAt 缺省 = 老版本记录，按未撤销） */
    const isRevoked = (device: DeviceDto) => device.revokedAt != null;

    /** M2 撤销入口仅非本机且未撤销行可见（本机不可用走「锁定设备」；内核另有 peerId/deviceUid 双重硬拒） */
    const canRevoke = (device: DeviceDto) => !device.isSelf && !isRevoked(device);

    /**
     * M2 撤销设备：确认对话框三条口径（m1-m2-implementation-plan §4.3，逐字稳定）→
     * devices.revoke；成功后列表经既有 DeviceUpdated 监听刷新（事件携带带 revokedAt
     * 的记录，监听为整单重载，灰态即时生效）。
     */
    const confirmRevoke = async (device: DeviceDto) => {
      try {
        await ElMessageBox.confirm(
          '撤销后该设备将立即断连，不再同步。撤销不会删除该设备上已有的数据，其已保存的聊天记录仍可查看。离线设备将在其下次尝试连接时失效。',
          `撤销设备『${device.deviceName}』？`,
          {
            type: 'warning',
            confirmButtonText: '撤销',
            cancelButtonText: '取消',
            confirmButtonType: 'danger'
          }
        );
      } catch {
        return; // 用户取消/关闭
      }
      try {
        await window.electronAPI.devices.revoke(device.peerId);
        // 撤销成功不弹 tips：走消息页 app:system 系统消息落一条可追溯记录
        notifyDeviceRevoked(spaceKeyOf(currentSpace.value), device.deviceName);
      } catch (error) {
        // 壳层命令返回 Result<T, String>：reject 值为内核错误文案串（KernelError Display 直出）
        const message = error instanceof Error ? error.message : String(error);
        if (message.includes('Cannot revoke current device')) {
          ElMessage.error('不能撤销当前设备：本机请使用「锁定设备」');
        } else if (message.includes('Device not found')) {
          ElMessage.error('设备不存在或已被移除');
        } else {
          ElMessage.error(`撤销失败：${message}`);
        }
      }
    };

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
      newJoinedPeerIds,
      deviceSummary,
      osLine,
      versionText,
      hasUpdate,
      isRevoked,
      canRevoke,
      confirmRevoke,
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

/* M2 已撤销行灰化：名称降到三级文字色（摘要行/图标已同为灰；行仍可点击查看详情） */
.device-revoked .mine-list-item-text b {
  color: var(--spark-text-3);
}

/* 行内撤销按钮：不随 .mine-list-item.active 变色，保持 danger 语义 */
.device-revoke-btn {
  flex-shrink: 0;
}

/* div 行容器的键盘焦点环（行容器非原生 button，补 :focus-visible 可见指示） */
.mine-list-item:focus-visible {
  outline: 2px solid var(--spark-primary);
  outline-offset: -2px;
}
</style>
