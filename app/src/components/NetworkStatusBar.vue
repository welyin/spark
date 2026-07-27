<template>
  <el-popover placement="bottom-end" :width="330" trigger="click">
    <template #reference>
      <button class="net-status-trigger" :title="statusLabel">
        <span class="net-status-dot" :class="`is-${statusKind}`" />
        <span class="net-status-label">{{ statusLabel }}</span>
      </button>
    </template>

    <div class="net-status-panel">
      <div class="net-status-panel-title">
        <span class="net-status-dot" :class="`is-${statusKind}`" />
        <b>{{ statusLabel }}</b>
      </div>
      <p class="net-status-desc">{{ statusDescription }}</p>

      <template v-if="overview">
        <div class="net-status-row">
          <span>组织</span>
          <b>{{ currentOrgName }}</b>
        </div>
        <div class="net-status-row">
          <span>已连接副本</span>
          <b>{{ overview.connectedPeers + 1 }}/{{ overview.replicaTarget }}（含本机）</b>
        </div>
        <div class="net-status-row">
          <span>已同步副本</span>
          <b>{{ overview.syncedPeers }}/{{ overview.totalMembers }}</b>
        </div>
        <div class="net-status-row">
          <span>最近同步</span>
          <b>{{ lastSyncedText }}</b>
        </div>
        <div class="net-status-row">
          <span>DHT 模式</span>
          <b>{{ dhtModeText }}</b>
        </div>
        <div v-if="overview.recoveryState !== 'idle'" class="net-status-row">
          <span>恢复状态</span>
          <b>{{ recoveryText }}</b>
        </div>
      </template>

      <template v-else>
        <div class="net-status-row">
          <span>网络连接</span>
          <b>{{ p2pInfo?.started ? `${p2pInfo.connectedPeers.length} 个节点` : '未启动' }}</b>
        </div>
        <p class="net-status-hint">加入或选中组织后，这里显示副本级状态。</p>
      </template>
    </div>
  </el-popover>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { listenP2pEvents, type OrgNetworkStatus, type OrgSyncOverviewDto, type OrgView } from '../api';
import { currentOrgId } from '../stores/current-org';

type P2pInfo = {
  initialized: boolean;
  started: boolean;
  peerId: string | null;
  addresses: string[];
  connectedPeers: string[];
  sparkSyncSubscribers: string[];
  error?: string | null;
};

const STATUS_LABELS: Record<OrgNetworkStatus, string> = {
  good: '组织网络良好',
  unstable: '网络不稳定',
  lost: '组织网络丢失',
  recovering: '正在恢复',
  localOnly: '仅本地'
};

/** 事件刷新最小间隔（连接/断开事件可能成对突发，避免连发 invoke）。 */
const EVENT_REFRESH_MIN_INTERVAL_MS = 2_000;
/** 轮询兜底周期。 */
const POLL_INTERVAL_MS = 30_000;

export default defineComponent({
  name: 'NetworkStatusBar',
  setup() {
    const organizations = ref<OrgView[]>([]);
    const overview = ref<OrgSyncOverviewDto | null>(null);
    const p2pInfo = ref<P2pInfo | null>(null);
    let pollTimer: ReturnType<typeof setInterval> | null = null;
    let unlisten: (() => void) | null = null;
    let lastEventRefreshAt = 0;

    // 当前组织：OrgPage 写入的选中项优先，否则兜底第一个组织
    const currentOrg = computed<OrgView | null>(() => {
      return (
        organizations.value.find((org) => org.orgId === currentOrgId.value) ??
        organizations.value[0] ??
        null
      );
    });
    const currentOrgName = computed(() => currentOrg.value?.name ?? '-');

    const statusKind = computed<OrgNetworkStatus>(() => {
      if (overview.value) {
        return overview.value.status;
      }
      // 无组织（或未选中）时的全局最简状态
      const info = p2pInfo.value;
      if (info?.started && info.connectedPeers.length > 0) {
        return 'good';
      }
      return 'localOnly';
    });

    const statusLabel = computed(() => {
      if (!overview.value && statusKind.value === 'good') {
        return '网络已连接';
      }
      return STATUS_LABELS[statusKind.value];
    });

    const statusDescription = computed(() => {
      switch (statusKind.value) {
        case 'good':
          return overview.value
            ? '可连接多数目标副本，数据同步正常。'
            : '已连接到网络。';
        case 'unstable':
          return '仅部分副本可连接（少于 K），数据同步可能延迟。';
        case 'lost':
          return overview.value?.recoveryState === 'failed'
            ? '所有已知成员地址连接失败，自动恢复暂无结果；请在组织详情页通过「恢复连接」手动添加节点。'
            : '所有已知成员地址连接失败，已自动进入恢复模式。';
        case 'recovering':
          return '正在通过 DHT / 恢复查询查找组织成员…';
        case 'localOnly':
          return '当前完全离线，仅本地可用；数据将在网络恢复后同步。';
      }
    });

    const lastSyncedText = computed(() => {
      const times = (overview.value?.members ?? [])
        .map((member) => member.lastSyncedAt)
        .filter((ts): ts is number => typeof ts === 'number');
      if (times.length === 0) {
        return '暂无记录';
      }
      return new Date(Math.max(...times)).toLocaleString();
    });

    const dhtModeText = computed(() => {
      switch (overview.value?.dhtMode) {
        case 'off':
          return '完全私有（已关闭）';
        case 'client':
          return '仅客户端';
        case 'server':
          return '开放（全量节点）';
        default:
          return '-';
      }
    });

    const recoveryText = computed(() => {
      const state = overview.value?.recoveryState;
      const since = overview.value?.recoveryStartedAt;
      const sinceText = since ? new Date(since).toLocaleTimeString() : '';
      if (state === 'recovering') {
        return `恢复查找中${sinceText ? `（始于 ${sinceText}）` : ''}`;
      }
      if (state === 'failed') {
        return '自动恢复暂无结果';
      }
      return '-';
    });

    const refreshOverview = async () => {
      const org = currentOrg.value;
      if (!org) {
        overview.value = null;
        return;
      }
      try {
        overview.value = await window.electronAPI.organization.getSyncOverview(org.orgId);
      } catch {
        // 状态读取失败不打扰用户，保留下一轮
      }
    };

    const refreshAll = async () => {
      try {
        organizations.value = await window.electronAPI.organization.listMine();
      } catch {
        // 同上
      }
      try {
        p2pInfo.value = await window.electronAPI.p2p.info();
      } catch {
        // 同上
      }
      await refreshOverview();
    };

    const onP2pEvent = () => {
      const now = Date.now();
      if (now - lastEventRefreshAt < EVENT_REFRESH_MIN_INTERVAL_MS) {
        return;
      }
      lastEventRefreshAt = now;
      void refreshOverview();
    };

    // 切换组织后旧 overview 立即作废，避免弹层出现「新组织名 + 旧 overview」错配（最长持续一个轮询周期）
    watch(currentOrgId, () => {
      overview.value = null;
      void refreshOverview();
    });

    onMounted(async () => {
      await refreshAll();
      pollTimer = setInterval(() => void refreshAll(), POLL_INTERVAL_MS);
      try {
        unlisten = await listenP2pEvents(onP2pEvent);
      } catch {
        // 非 Tauri 环境（单测/旧壳）无事件通道，仅靠轮询
      }
    });

    onBeforeUnmount(() => {
      if (pollTimer) {
        clearInterval(pollTimer);
      }
      unlisten?.();
    });

    return {
      overview,
      p2pInfo,
      currentOrgName,
      statusKind,
      statusLabel,
      statusDescription,
      lastSyncedText,
      dhtModeText,
      recoveryText
    };
  }
});
</script>

<style scoped>
.net-status-trigger {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  font-size: 12px;
  color: var(--spark-text-2);
  padding: 4px 8px;
  border-radius: var(--spark-radius-l);
  -webkit-app-region: no-drag;
}

.net-status-trigger:hover {
  background: var(--spark-bg-hover);
}

.net-status-dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  flex-shrink: 0;
}

.net-status-dot.is-good {
  background: var(--el-color-success);
}

.net-status-dot.is-unstable {
  background: var(--el-color-warning);
}

.net-status-dot.is-lost {
  background: var(--el-color-danger);
}

.net-status-dot.is-recovering {
  background: #ee8c2b;
}

.net-status-dot.is-localOnly {
  background: var(--el-color-info);
}

.net-status-panel-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}

.net-status-desc {
  margin: 8px 0 10px;
  font-size: 12px;
  color: var(--spark-text-2);
  line-height: 1.5;
}

.net-status-row {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  font-size: 12px;
  padding: 3px 0;
  color: var(--spark-text-2);
}

.net-status-row b {
  color: var(--spark-text-1);
  font-weight: 500;
  text-align: right;
  word-break: break-all;
}

.net-status-hint {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--spark-text-3);
}
</style>
