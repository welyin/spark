<template>
  <el-popover placement="bottom-end" :width="330" trigger="hover">
    <template #reference>
      <button class="net-status-tag" :class="`is-${statusKind}`" :title="statusLabel">
        <span class="net-status-dot" :class="`is-${statusKind}`" />
        <span class="net-status-label">{{ statusLabel }}</span>
        <span class="net-status-count">{{ peerCountText }}</span>
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
        <p class="net-status-hint">加入或切换到组织空间后，这里显示副本级状态。</p>
      </template>
    </div>
  </el-popover>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { listenP2pEvents, type OrgNetworkStatus, type OrgSyncOverviewDto, type OrgView, type P2pEventDto } from '../api';
import { currentOrgId } from '../stores/current-org';
import { findOrg, refreshOrganizations } from '../stores/org-membership';
import { refreshNetworkStatus, useNetworkStatus } from '../stores/network-status';

const STATUS_LABELS: Record<OrgNetworkStatus, string> = {
  good: '组织网络良好',
  unstable: '网络不稳定',
  lost: '组织网络丢失',
  recovering: '正在恢复',
  localOnly: '仅本地'
};

/** 组织 overview 事件刷新最小间隔（连接/断开事件可能成对突发，避免连发 invoke）。 */
const EVENT_REFRESH_MIN_INTERVAL_MS = 2_000;
/** 轮询兜底周期。 */
const POLL_INTERVAL_MS = 30_000;

export default defineComponent({
  name: 'NetworkStatusBar',
  setup() {
    const overview = ref<OrgSyncOverviewDto | null>(null);
    // 全局 P2P 状态共享自 network-status store（统一轮询，避免与消息页等各自调接口）
    const { p2pInfoSnapshot: p2pInfo, connectedPeerCount } = useNetworkStatus();
    let pollTimer: ReturnType<typeof setInterval> | null = null;
    let unlisten: (() => void) | null = null;
    let lastEventRefreshAt = 0;

    // 当前组织：只跟随当前空间（currentOrgId 由 currentSpace 派生）；
    // 个人空间（空串）不再回退第一个组织，弹层显示全局 P2P 状态（ui-space-navbar §4.1）
    const currentOrg = computed<OrgView | null>(() => {
      return findOrg(currentOrgId.value);
    });
    const currentOrgName = computed(() => currentOrg.value?.name ?? '-');

    const statusKind = computed<OrgNetworkStatus>(() => {
      if (overview.value) {
        return overview.value.status;
      }
      // 无组织（或未选中）时的全局最简状态：
      // 未启动=仅本地；已启动未连上节点=unstable（弱提示，不阻断操作）；已连接=good
      const info = p2pInfo.value;
      if (!info?.started) {
        return 'localOnly';
      }
      return info.connectedPeers.length > 0 ? 'good' : 'unstable';
    });

    const statusLabel = computed(() => {
      if (!overview.value && statusKind.value === 'good') {
        return '网络已连接';
      }
      if (!overview.value && statusKind.value === 'unstable') {
        return '暂未连接节点';
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
          return overview.value
            ? '仅部分副本可连接（少于 K），数据同步可能延迟。'
            : 'P2P 已启动，暂未连接到任何节点；请求将照常发出，送达失败可重试。';
        case 'lost':
          return overview.value?.recoveryState === 'failed'
            ? '所有已知成员地址连接失败，自动恢复暂无结果；请在组织详情页通过「恢复连接」手动添加节点。'
            : '所有已知成员地址连接失败，已自动进入恢复模式。';
        case 'recovering':
          return '正在通过 DHT / 恢复查询查找组织成员…';
        case 'localOnly':
          return 'P2P 未启动，当前仅本地可用；数据将在网络恢复后同步。';
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

    /** 胶囊上的数量：组织空间=已同步成员数，个人空间=已连接节点数 */
    const peerCountText = computed(() => {
      if (overview.value) {
        return `${overview.value.syncedPeers} 成员已同步`;
      }
      return `${connectedPeerCount.value} 节点`;
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
        await refreshOrganizations();
      } catch {
        // 同上
      }
      await refreshNetworkStatus();
      await refreshOverview();
    };

    const onP2pEvent = (event: P2pEventDto) => {
      const now = Date.now();
      if (now - lastEventRefreshAt < EVENT_REFRESH_MIN_INTERVAL_MS) {
        return;
      }
      lastEventRefreshAt = now;
      void refreshOverview();
      // 组织快照被接受落库（名称/logo 等可能已变）：立即刷新组织列表缓存，
      // 不等下一轮 30s 轮询
      if (event.kind === 'OrgShareAccepted') {
        void refreshOrganizations().catch(() => {});
      }
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
      peerCountText,
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
/* 状态胶囊：在线=绿、弱网/部分离线/同步中=黄、离线=红、未启动/仅本地=灰 */
.net-status-tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  border: 0;
  cursor: pointer;
  font-family: inherit;
  font-size: var(--spark-font-size-secondary);
  line-height: 1.4;
  padding: 3px 10px;
  border-radius: 999px;
  -webkit-app-region: no-drag;
}

.net-status-tag:hover {
  opacity: 0.85;
}

.net-status-tag.is-good {
  background: var(--spark-success-bg);
  color: var(--spark-success);
}

.net-status-tag.is-unstable,
.net-status-tag.is-recovering {
  background: var(--spark-warning-bg);
  color: var(--spark-warning);
}

.net-status-tag.is-lost {
  background: var(--spark-danger-bg);
  color: var(--spark-danger);
}

.net-status-tag.is-localOnly {
  background: var(--spark-bg-hover);
  color: var(--spark-text-2);
}

.net-status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

/* 常驻数量（已连接节点数/已同步成员数）：比状态文字弱一级 */
.net-status-count {
  opacity: 0.75;
  font-size: 11px;
}

.net-status-count::before {
  content: '·';
  margin-right: 4px;
}

.net-status-dot.is-good {
  background: var(--spark-success);
}

.net-status-dot.is-unstable,
.net-status-dot.is-recovering {
  background: var(--spark-warning);
}

.net-status-dot.is-lost {
  background: var(--spark-danger);
}

.net-status-dot.is-localOnly {
  background: var(--spark-text-3);
}

.net-status-panel-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: var(--spark-font-size-placeholder);
}

.net-status-desc {
  margin: 8px 0 10px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-2);
  line-height: 1.5;
}

.net-status-row {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  font-size: var(--spark-font-size-secondary);
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
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}
</style>
