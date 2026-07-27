<template>
  <!-- 节点状态卡片 -->
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>节点状态</h2>
    </template>
    <el-descriptions :column="1" border>
      <el-descriptions-item label="RootID">{{ rootId || '未创建' }}</el-descriptions-item>
      <el-descriptions-item label="状态">已登录</el-descriptions-item>
      <el-descriptions-item label="P2P 初始化">{{ p2pInfo.initialized ? '是' : '否' }}</el-descriptions-item>
      <el-descriptions-item label="P2P 运行中">{{ p2pInfo.started ? '是' : '否' }}</el-descriptions-item>
      <el-descriptions-item label="PeerId">{{ p2pInfo.peerId || '未获取' }}</el-descriptions-item>
      <el-descriptions-item label="已连接 Peer">
        <template v-if="p2pInfo.connectedPeers.length > 0">
          <div v-for="peer in p2pInfo.connectedPeers" :key="peer" class="mono">{{ peer }}</div>
        </template>
        <span v-else>暂无</span>
      </el-descriptions-item>
      <el-descriptions-item label="spark-sync 订阅者">
        <template v-if="p2pInfo.sparkSyncSubscribers.length > 0">
          <div v-for="peer in p2pInfo.sparkSyncSubscribers" :key="`sub-${peer}`" class="mono">{{ peer }}</div>
        </template>
        <span v-else>暂无</span>
      </el-descriptions-item>
      <el-descriptions-item label="节点地址">
        <template v-if="p2pInfo.addresses.length > 0">
          <div v-for="addr in p2pInfo.addresses" :key="addr" class="mono">{{ addr }}</div>
        </template>
        <span v-else>未获取（可能仍在启动或未监听可拨号地址）</span>
      </el-descriptions-item>
    </el-descriptions>

    <el-alert
      v-if="p2pInfo.error"
      :title="`P2P 启动异常：${p2pInfo.error}`"
      type="warning"
      :closable="false"
      show-icon
      class="block-gap"
    />
  </el-card>

  <!-- DHT 隐私开关卡片 -->
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>网络</h2>
    </template>
    <p class="hint">
      DHT（分布式节点发现）帮助节点在公网找到彼此。完全私有模式下节点不参与公共 DHT，
      仅依靠直连与局域网发现，组织外节点无法经 DHT 发现你。
    </p>
    <el-radio-group
      :model-value="dhtMode"
      :disabled="dhtModeSaving"
      @change="onDhtModeChange"
    >
      <el-radio value="server">开放（默认）：参与公共 DHT 节点发现</el-radio>
      <el-radio value="off">完全私有：关闭 DHT</el-radio>
    </el-radio-group>
    <el-alert
      v-if="dhtModeMessage"
      :title="dhtModeMessage"
      type="info"
      :closable="false"
      show-icon
      class="block-gap"
    />
  </el-card>
</template>

<script lang="ts">
/**
 * 网络状态卡片（自 MinePage.vue 抽出，纯结构移动）。
 *
 * 职责：P2P 节点状态展示（p2pInfo 由父组件 MinePage 持有并刷新，本组件只读展示）
 * + DHT 隐私两档开关（p2p.getDhtMode/setDhtMode，本组件自管状态）。
 */
import { defineComponent, onMounted, ref, type PropType } from 'vue';
import { errorMessage } from '../utils/ipc';

type P2PInfo = {
  initialized: boolean;
  started: boolean;
  peerId: string | null;
  addresses: string[];
  connectedPeers: string[];
  sparkSyncSubscribers: string[];
  error?: string | null;
};

type DhtMode = 'off' | 'client' | 'server';

export default defineComponent({
  name: 'NetworkCard',
  props: {
    rootId: {
      type: String,
      default: ''
    },
    p2pInfo: {
      type: Object as PropType<P2PInfo>,
      required: true
    }
  },
  setup() {
    // ---------------- 网络（DHT 隐私开关） ----------------
    const dhtMode = ref<DhtMode>('server');
    const dhtModeSaving = ref(false);
    const dhtModeMessage = ref('');

    const refreshDhtMode = async () => {
      try {
        const result = await window.electronAPI.p2p.getDhtMode();
        dhtMode.value = result.dhtMode === 'off' ? 'off' : 'server';
      } catch {
        // 读取失败保持默认展示（开放）
      }
    };

    const onDhtModeChange = async (value: string | number | boolean) => {
      const mode: DhtMode = value === 'off' ? 'off' : 'server';
      dhtModeSaving.value = true;
      try {
        const result = await window.electronAPI.p2p.setDhtMode(mode);
        dhtMode.value = result.dhtMode === 'off' ? 'off' : 'server';
        dhtModeMessage.value = '已保存；若 P2P 正在运行，节点会自动重启以应用新模式';
      } catch (error) {
        dhtModeMessage.value = `保存失败：${errorMessage(error)}`;
      } finally {
        dhtModeSaving.value = false;
      }
    };

    onMounted(refreshDhtMode);

    return {
      dhtMode,
      dhtModeSaving,
      dhtModeMessage,
      onDhtModeChange
    };
  }
});
</script>
