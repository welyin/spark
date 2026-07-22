<template>
  <section class="card-page">
    <header class="page-header">
      <div class="page-header-main">
        <p class="eyebrow">开发调试</p>
        <h1>测试</h1>
        <p class="lede">数据库、节点与更新链路的调试面板。</p>
      </div>
    </header>

    <el-card class="node-panel">
      <div class="panel-title">
        <div>
          <h2>节点面板</h2>
          <p class="subtitle">展示本地保存的所有节点数据，并可按节点触发组织同步。</p>
        </div>
        <div class="panel-actions">
          <el-button type="danger" plain @click="clearNodes" :loading="clearingNodes" :disabled="loadingNodes || clearingNodes">
            {{ clearingNodes ? '清空中...' : '清空所有节点' }}
          </el-button>
          <el-button type="primary" @click="refreshNodes" :loading="loadingNodes" :disabled="loadingNodes || clearingNodes">
            {{ loadingNodes ? '刷新中...' : '刷新节点' }}
          </el-button>
        </div>
      </div>

      <el-alert v-if="nodeMessage" class="message" :title="nodeMessage" type="info" :closable="false" show-icon />
      <div v-if="loadingNodes" class="empty-state">正在加载节点...</div>
      <div v-else-if="nodeRecords.length === 0" class="empty-state">暂无保存的节点记录。</div>
      <div v-else class="node-grid">
        <el-card v-for="node in nodeRecords" :key="node.nodeKey" class="node-card" shadow="never">
          <div class="node-card-head">
            <strong>{{ node.peerId || '未解析 PeerId' }}</strong>
            <el-button type="success" @click="syncNode(node)" :loading="syncingNodeKey === node.nodeKey" :disabled="syncingNodeKey === node.nodeKey">
              {{ syncingNodeKey === node.nodeKey ? '同步中...' : '同步' }}
            </el-button>
          </div>
          <p class="muted">{{ node.addresses.length }} 个地址 · 最近 {{ formatDate(node.lastSeenAt) }}</p>
          <div class="node-meta">
            <el-tag effect="plain">成功 {{ node.successCount }}</el-tag>
            <el-tag type="danger" effect="plain">失败 {{ node.failureCount }}</el-tag>
            <el-tag type="info" effect="plain">累计在线 {{ formatDuration(node.cumulativeConnectedMs) }}</el-tag>
          </div>
          <p v-if="node.lastError" class="error-text">最后错误：{{ node.lastError }}</p>
          <p class="address-text">{{ node.addresses.join(' , ') }}</p>
        </el-card>
      </div>
    </el-card>

    <el-card class="updater-panel">
      <div class="panel-title">
        <div>
          <h2>更新调试面板</h2>
          <p class="subtitle">用于联调 GitHub Releases 更新链路：检查、下载、应用重启。</p>
        </div>
        <el-button @click="refreshUpdaterStatus" :loading="loadingUpdater">刷新状态</el-button>
      </div>

      <el-alert v-if="updaterMessage" class="message" :title="updaterMessage" type="info" :closable="false" show-icon />

      <el-descriptions :column="1" border>
        <el-descriptions-item label="是否已配置">{{ updaterStatus?.configured ? '是' : '否' }}</el-descriptions-item>
        <el-descriptions-item label="应用标识">{{ updaterStatus?.appId || '-' }}</el-descriptions-item>
        <el-descriptions-item label="通道">{{ updaterStatus?.channel || '-' }}</el-descriptions-item>
        <el-descriptions-item label="当前版本">{{ updaterStatus?.currentVersion || '-' }}</el-descriptions-item>
        <el-descriptions-item label="最高接受版本">{{ updaterStatus?.highestAcceptedVersion || '-' }}</el-descriptions-item>
        <el-descriptions-item label="最近检查">
          <template v-if="updaterStatus?.latestCheck">
            {{ formatDate(updaterStatus.latestCheck.checkedAt) }} · {{ updaterStatus.latestCheck.reason }}
          </template>
          <template v-else>暂无</template>
        </el-descriptions-item>
        <el-descriptions-item label="可用版本">{{ updaterStatus?.latestCheck?.availableVersion || '无' }}</el-descriptions-item>
        <el-descriptions-item label="已暂存安装包">
          <template v-if="updaterStatus?.staged">
            {{ updaterStatus.staged.fileName }} ({{ updaterStatus.staged.version }})
          </template>
          <template v-else>无</template>
        </el-descriptions-item>
      </el-descriptions>

      <div class="row">
        <el-button type="primary" @click="checkUpdates" :loading="checkingUpdate" :disabled="checkingUpdate">检查更新</el-button>
        <el-button @click="stageLatestUpdate" :loading="stagingUpdate" :disabled="stagingUpdate">下载并校验</el-button>
        <el-button type="danger" plain @click="applyUpdateRestart" :loading="applyingUpdate" :disabled="applyingUpdate">
          应用并重启
        </el-button>
      </div>

      <el-card shadow="never" class="inner-card">
        <template #header>
          <h3>对端版本观测</h3>
        </template>
        <div v-if="!updaterStatus?.peerObservations?.length" class="empty-state">暂无观测记录。</div>
        <el-table v-else :data="updaterStatus.peerObservations" size="small" stripe>
          <el-table-column prop="peerId" label="PeerId" min-width="220" />
          <el-table-column prop="observedVersion" label="对端版本" width="130" />
          <el-table-column label="观测时间" min-width="170">
            <template #default="scope">{{ formatDate(scope.row.observedAt) }}</template>
          </el-table-column>
          <el-table-column label="触发检查" width="100">
            <template #default="scope">
              <el-tag :type="scope.row.triggeredCheck ? 'success' : 'info'" effect="plain">
                {{ scope.row.triggeredCheck ? '是' : '否' }}
              </el-tag>
            </template>
          </el-table-column>
        </el-table>
      </el-card>
    </el-card>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ElMessageBox } from 'element-plus';

type SavedNodeRecord = {
  nodeKey: string;
  peerId: string | null;
  addresses: string[];
  firstSeenAt: number;
  lastSeenAt: number;
  lastConnectedAt: number | null;
  lastDisconnectedAt: number | null;
  successCount: number;
  failureCount: number;
  cumulativeConnectedMs: number;
  currentSessionConnectedAt?: number;
  lastError?: string;
};

type UpdaterStatus = {
  configured: boolean;
  appId: string;
  channel: 'stable' | 'canary';
  currentVersion: string;
  highestAcceptedVersion: string;
  latestCheck: {
    checkedAt: number;
    source: 'manual' | 'startup' | 'peer-observed';
    currentVersion: string;
    availableVersion: string | null;
    updateAvailable: boolean;
    critical: boolean;
    revokedCurrentVersion: boolean;
    reason: string;
  } | null;
  staged: {
    version: string;
    filePath: string;
    fileName: string;
    sha256: string;
    size: number;
    stagedAt: number;
  } | null;
  peerObservations: Array<{
    peerId: string;
    observedVersion: string;
    observedAt: number;
    triggeredCheck: boolean;
  }>;
};

export default defineComponent({
  name: 'TestPage',
  setup() {
    const nodeRecords = ref<SavedNodeRecord[]>([]);
    const loadingNodes = ref(false);
    const clearingNodes = ref(false);
    const syncingNodeKey = ref('');
    const nodeMessage = ref('');
    const updaterStatus = ref<UpdaterStatus | null>(null);
    const updaterMessage = ref('');
    const loadingUpdater = ref(false);
    const checkingUpdate = ref(false);
    const stagingUpdate = ref(false);
    const applyingUpdate = ref(false);

    const parseNodeRecord = (key: string, value: string): SavedNodeRecord | null => {
      try {
        const record = JSON.parse(value) as Omit<SavedNodeRecord, 'nodeKey'>;
        return {
          nodeKey: key,
          peerId: record.peerId ?? null,
          addresses: Array.isArray(record.addresses) ? record.addresses : [],
          firstSeenAt: record.firstSeenAt,
          lastSeenAt: record.lastSeenAt,
          lastConnectedAt: record.lastConnectedAt ?? null,
          lastDisconnectedAt: record.lastDisconnectedAt ?? null,
          successCount: record.successCount ?? 0,
          failureCount: record.failureCount ?? 0,
          cumulativeConnectedMs: record.cumulativeConnectedMs ?? 0,
          currentSessionConnectedAt: record.currentSessionConnectedAt,
          lastError: record.lastError
        };
      } catch {
        return null;
      }
    };

    const refreshNodes = async () => {
      loadingNodes.value = true;
      nodeMessage.value = '';
      try {
        const rows = await window.electronAPI.db.query('p2p:peer:record:');
        nodeRecords.value = rows
          .map((row) => parseNodeRecord(row.key, row.value))
          .filter((row): row is SavedNodeRecord => row !== null)
          .sort((left, right) => right.lastSeenAt - left.lastSeenAt);
      } catch (error) {
        nodeMessage.value = `加载节点失败：${error}`;
      } finally {
        loadingNodes.value = false;
      }
    };

    const syncNode = async (node: SavedNodeRecord) => {
      syncingNodeKey.value = node.nodeKey;
      nodeMessage.value = '';
      try {
        const targetPeer = {
          peerId: node.peerId ?? undefined,
          addresses: [...node.addresses]
        };
        const result = await window.electronAPI.p2p.syncPeerOrganizations(targetPeer);
        nodeMessage.value = `节点同步完成：推送尝试 ${result.attempted} / 成功 ${result.synced}；拉取检查 ${result.pullChecked} / 更新 ${result.pullSynced}；移除本地组织 ${result.removed}`;
      } catch (error) {
        nodeMessage.value = `节点同步失败：${error}`;
      } finally {
        syncingNodeKey.value = '';
      }
    };

    const clearNodes = async () => {
      try {
        await ElMessageBox.confirm('确认清空所有已保存节点记录？该操作仅影响本机缓存。', '清空确认', {
          type: 'warning',
          confirmButtonText: '确认清空',
          cancelButtonText: '取消'
        });
      } catch {
        return;
      }

      clearingNodes.value = true;
      nodeMessage.value = '';
      try {
        const result = await window.electronAPI.p2p.clearPeerRecords();
        nodeMessage.value = `已清空节点记录：${result.cleared} 条`;
        await refreshNodes();
      } catch (error) {
        nodeMessage.value = `清空节点失败：${error}`;
      } finally {
        clearingNodes.value = false;
      }
    };

    const refreshUpdaterStatus = async () => {
      loadingUpdater.value = true;
      try {
        updaterStatus.value = await window.electronAPI.updater.status();
      } catch (error) {
        updaterMessage.value = `读取更新状态失败：${error}`;
      } finally {
        loadingUpdater.value = false;
      }
    };

    const checkUpdates = async () => {
      checkingUpdate.value = true;
      updaterMessage.value = '';
      try {
        const result = await window.electronAPI.updater.check();
        updaterMessage.value = result.updateAvailable
          ? `检测到新版本：${result.availableVersion}`
          : '当前已是最新版本';
        await refreshUpdaterStatus();
      } catch (error) {
        updaterMessage.value = `检查更新失败：${error}`;
      } finally {
        checkingUpdate.value = false;
      }
    };

    const stageLatestUpdate = async () => {
      stagingUpdate.value = true;
      updaterMessage.value = '';
      try {
        const staged = await window.electronAPI.updater.stageLatest();
        updaterMessage.value = `已下载并校验：${staged.fileName}`;
        await refreshUpdaterStatus();
      } catch (error) {
        updaterMessage.value = `下载更新失败：${error}`;
      } finally {
        stagingUpdate.value = false;
      }
    };

    const applyUpdateRestart = async () => {
      applyingUpdate.value = true;
      updaterMessage.value = '';
      try {
        await window.electronAPI.updater.applyRestart();
        updaterMessage.value = '正在重启应用以安装更新...';
      } catch (error) {
        updaterMessage.value = `应用更新失败：${error}`;
        applyingUpdate.value = false;
      }
    };

    const formatDate = (timestamp: number) => {
      return new Intl.DateTimeFormat('zh-CN', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
      }).format(new Date(timestamp));
    };

    const formatDuration = (durationMs: number) => {
      const minutes = Math.floor(durationMs / 60000);
      const hours = Math.floor(minutes / 60);
      if (hours > 0) {
        return `${hours} 小时 ${minutes % 60} 分钟`;
      }
      return `${minutes} 分钟`;
    };

    onMounted(() => {
      void refreshNodes();
      void refreshUpdaterStatus();
    });

    return {
      nodeRecords,
      loadingNodes,
      clearingNodes,
      syncingNodeKey,
      nodeMessage,
      updaterStatus,
      updaterMessage,
      loadingUpdater,
      checkingUpdate,
      stagingUpdate,
      applyingUpdate,
      refreshNodes,
      clearNodes,
      syncNode,
      refreshUpdaterStatus,
      checkUpdates,
      stageLatestUpdate,
      applyUpdateRestart,
      formatDate,
      formatDuration
    };
  }
});
</script>

<style scoped src="../styles/pages/test.css"></style>
