<!-- 网络状态模块（MinePage「网络状态」第三、四栏）：
     第三栏=状态分类（节点信息/连接状态/DHT 网络/同步状态）；
     「节点信息」直接展示全部细节（身份 ID、节点 ID、地址、运行状态等），其余分类
     默认普通用户视图，「高级模式」开关（localStorage 记忆）打开后才显示技术细节 -->
<template>
  <!-- 第三栏：状态分类 -->
  <div class="mine-list">
    <h2 class="mine-list-title">网络状态</h2>
    <div class="mine-list-items">
      <button
        v-for="cat in categories"
        :key="cat.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeCategory === cat.key }"
        @click="activeCategory = cat.key"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="isMobileLayout ? { color: cat.color } : undefined"
        ><component :is="cat.icon" /></el-icon>
        <span class="mine-list-item-text">
          <b>{{ cat.label }}</b>
          <span>{{ cat.summary }}</span>
        </span>
      </button>
    </div>
  </div>

  <!-- 详情：column 模式=第四栏；drawer 模式=抽屉（设置页复用） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeCategory !== null"
    :title="currentCategoryLabel"
    @close="activeCategory = null"
  >
    <el-card v-if="activeCategory !== null" shadow="never" class="panel-card">
      <template #header>
        <div class="network-detail-header">
          <h2>{{ currentCategory.label }}</h2>
          <!-- 高级模式只作用于 P2P 状态分类，代理设置页隐藏 -->
          <span v-if="activeCategory !== 'proxy'" class="network-advanced">
            高级模式
            <el-switch :model-value="advanced" size="small" @change="toggleAdvanced" />
          </span>
        </div>
      </template>

      <el-alert
        v-if="p2pInfo.error"
        :title="`P2P 启动异常：${p2pInfo.error}`"
        type="warning"
        :closable="false"
        show-icon
        class="block-gap"
      />

      <!-- 节点信息：全部细节直接展示（身份 ID + 节点 ID/地址 + 运行状态） -->
      <template v-if="activeCategory === 'identity'">
        <div class="network-simple">
          <el-tag :type="rootId ? 'success' : 'info'">{{ rootId ? '已登录' : '未登录' }}</el-tag>
          <p class="hint">管理员添加你为成员时需要身份 ID 与节点信息。</p>
        </div>
        <NodeIdentityInfo class="network-rows" :rows="identityRows" />
        <div class="network-actions">
          <el-button type="primary" :disabled="!rootId" @click="copyRootId">复制身份 ID</el-button>
          <el-button @click="copyShareText">复制全部资料</el-button>
        </div>
      </template>

      <!-- 连接状态 -->
      <template v-else-if="activeCategory === 'connection'">
        <div class="network-simple">
          <el-tag :type="connected ? 'success' : 'info'">{{ connected ? '已连接' : '未连接' }}</el-tag>
          <span class="network-simple-text">已连接节点 {{ p2pInfo.connectedPeers.length }} 个</span>
        </div>
        <NodeIdentityInfo v-if="advanced" class="network-rows" :rows="connectionRows" />
        <div class="network-actions">
          <el-button text @click="emit('refresh')">刷新节点信息</el-button>
        </div>
      </template>

      <!-- DHT 网络 -->
      <template v-else-if="activeCategory === 'dht'">
        <p class="hint">
          节点发现帮助你的设备在网络中找到朋友和成员。完全私有模式下仅依靠直连与局域网发现，组织外节点无法发现你。
        </p>
        <el-radio-group
          :model-value="dhtMode"
          :disabled="dhtModeSaving"
          class="network-dht"
          @change="onDhtModeChange"
        >
          <el-radio value="server">开放（默认）：参与公共节点发现</el-radio>
          <el-radio value="off">完全私有：关闭节点发现</el-radio>
        </el-radio-group>
        <template v-if="advanced">
          <p class="hint">
            DHT（分布式节点发现）帮助节点在公网找到彼此。完全私有模式下节点不参与公共 DHT。
          </p>
          <NodeIdentityInfo class="network-rows" :rows="dhtRows" />
        </template>
        <el-alert
          v-if="dhtModeMessage"
          :title="dhtModeMessage"
          type="info"
          :closable="false"
          show-icon
          class="block-gap"
        />
      </template>

      <!-- 网络代理（HTTP 代理设置，真实生效；仅 show-proxy 时该分类存在） -->
      <ProxySettings v-else-if="activeCategory === 'proxy'" />

      <!-- 同步状态 -->
      <template v-else>
        <div class="network-simple">
          <el-tag :type="syncHealthy ? 'success' : 'warning'">{{ syncHealthy ? '同步状态良好' : '等待同步' }}</el-tag>
          <span v-if="!syncHealthy" class="network-simple-text">暂无其他设备/成员与你保持同步</span>
        </div>
        <NodeIdentityInfo v-if="advanced" class="network-rows" :rows="syncRows" />
      </template>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Connection, Key, Link, Refresh, Share } from '@element-plus/icons-vue';
import { errorMessage } from '../../utils/ipc';
import { isMobileLayout } from '../../stores/ui-layout';
import NodeIdentityInfo, { type NodeIdentityRow } from '../common/NodeIdentityInfo.vue';
import ProxySettings from '../settings/ProxySettings.vue';
import type { P2pInfoDto as P2PInfo } from '../../api';
import MineDetailContainer from './MineDetailContainer.vue';

type DhtMode = 'off' | 'client' | 'server';

type CategoryKey = 'identity' | 'connection' | 'dht' | 'sync' | 'proxy';

/** 高级模式开关持久化 key（localStorage） */
const ADVANCED_KEY = 'spark:network-advanced';

function readAdvanced(): boolean {
  try {
    return localStorage.getItem(ADVANCED_KEY) === '1';
  } catch {
    return false;
  }
}

export default defineComponent({
  name: 'NetworkModule',
  components: { NodeIdentityInfo, MineDetailContainer, ProxySettings },
  props: {
    rootId: { type: String, default: '' },
    p2pInfo: { type: Object as PropType<P2PInfo>, required: true },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' },
    /** 是否显示「网络代理」分类（HTTP 代理设置，真实生效；仅系统设置页开启） */
    showProxy: { type: Boolean, default: false }
  },
  emits: ['refresh'],
  setup(props, { emit }) {
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持默认选中「节点信息」
    const activeCategory = ref<CategoryKey | null>(props.detailMode === 'drawer' ? null : 'identity');

    // ---------------- 高级模式（localStorage 记忆） ----------------
    const advanced = ref(readAdvanced());
    const toggleAdvanced = (value: string | number | boolean) => {
      advanced.value = value === true;
      try {
        localStorage.setItem(ADVANCED_KEY, advanced.value ? '1' : '0');
      } catch {
        // 存储不可用时仅本次会话生效
      }
    };

    // ---------------- 普通视图摘要 ----------------
    const connected = computed(() => props.p2pInfo.started && props.p2pInfo.connectedPeers.length > 0);
    const syncHealthy = computed(() => props.p2pInfo.sparkSyncSubscribers.length > 0);

    // color 为移动端菜单图标色（微信式每项一色，与 MinePage 一级菜单同规则、同色系色板，桌面端不使用）
    const categories = computed<Array<{ key: CategoryKey; label: string; summary: string; icon: Component; color: string }>>(() => {
      const list: Array<{ key: CategoryKey; label: string; summary: string; icon: Component; color: string }> = [
        { key: 'identity', label: '节点信息', summary: props.rootId ? '已登录' : '未登录', icon: Key, color: '#64748b' },
        {
          key: 'connection',
          label: '连接状态',
          summary: connected.value ? `已连接 · 节点 ${props.p2pInfo.connectedPeers.length} 个` : '未连接',
          icon: Connection,
          color: '#3296fa'
        },
        { key: 'dht', label: 'DHT 网络', summary: dhtMode.value === 'off' ? '完全私有' : '开放', icon: Share, color: '#00b8a9' },
        { key: 'sync', label: '同步状态', summary: syncHealthy.value ? '同步状态良好' : '等待同步', icon: Refresh, color: '#34c19b' }
      ];
      // HTTP 代理设置（仅系统设置页传入 show-proxy 时出现）
      if (props.showProxy) {
        list.push({ key: 'proxy', label: '网络代理', summary: '更新与市场下载加速', icon: Link, color: '#ff7d00' });
      }
      return list;
    });

    const currentCategory = computed(
      () => categories.value.find((cat) => cat.key === activeCategory.value) ?? categories.value[0]
    );

    /** 抽屉标题：当前选中分类名 */
    const currentCategoryLabel = computed(() => (activeCategory.value ? currentCategory.value.label : ''));

    // ---------------- 节点信息行（「节点信息」分类始终展示全部细节；TermLabel 通俗名 + 悬停解释） ----------------
    const identityRows = computed<NodeIdentityRow[]>(() => [
      { label: '身份 ID', term: 'rootId', value: props.rootId, copyable: true, emptyText: '未创建' },
      { label: '节点 ID', term: 'peerId', value: props.p2pInfo.peerId ?? '' },
      {
        label: '节点地址',
        term: 'addresses',
        value: props.p2pInfo.addresses,
        emptyText: '未获取（可能仍在启动或未监听可拨号地址）'
      },
      { label: 'P2P 初始化', value: props.p2pInfo.initialized ? '是' : '否' },
      { label: 'P2P 运行中', value: props.p2pInfo.started ? '是' : '否' },
      { label: '已连接节点', value: props.p2pInfo.connectedPeers, emptyText: '暂无' },
      { label: 'spark-sync 订阅者', value: props.p2pInfo.sparkSyncSubscribers, emptyText: '暂无' }
    ]);

    const connectionRows = computed<NodeIdentityRow[]>(() => [
      { label: 'P2P 初始化', value: props.p2pInfo.initialized ? '是' : '否' },
      { label: 'P2P 运行中', value: props.p2pInfo.started ? '是' : '否' },
      { label: '节点 ID', term: 'peerId', value: props.p2pInfo.peerId ?? '' },
      { label: '已连接节点', value: props.p2pInfo.connectedPeers, emptyText: '暂无' },
      { label: '节点地址', term: 'addresses', value: props.p2pInfo.addresses, emptyText: '未获取' }
    ]);

    const dhtRows = computed<NodeIdentityRow[]>(() => [
      { label: 'DHT 模式', value: dhtMode.value === 'off' ? 'off（完全私有）' : 'server（开放）' }
    ]);

    const syncRows = computed<NodeIdentityRow[]>(() => [
      { label: 'spark-sync 订阅者', value: props.p2pInfo.sparkSyncSubscribers, emptyText: '暂无' }
    ]);

    // ---------------- 复制 ----------------
    const copyRootId = async () => {
      if (!props.rootId) {
        return;
      }
      try {
        await navigator.clipboard.writeText(props.rootId);
        ElMessage.success('身份 ID 已复制');
      } catch (error) {
        ElMessage.error(`复制失败：${error}`);
      }
    };

    const copyShareText = async () => {
      const addressesText = props.p2pInfo.addresses.length > 0 ? props.p2pInfo.addresses.join('\n') : '未获取';
      const text = [
        `RootID: ${props.rootId || '未创建'}`,
        `PeerId: ${props.p2pInfo.peerId || '未获取'}`,
        'P2P Addresses:',
        addressesText
      ].join('\n');
      try {
        await navigator.clipboard.writeText(text);
        ElMessage.success('节点身份信息已复制');
      } catch (error) {
        ElMessage.error(`复制失败：${error}`);
      }
    };

    // ---------------- DHT 隐私开关（与 NetworkCard 同一套 getDhtMode/setDhtMode 逻辑） ----------------
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
      activeCategory,
      advanced,
      toggleAdvanced,
      connected,
      syncHealthy,
      categories,
      currentCategory,
      currentCategoryLabel,
      identityRows,
      connectionRows,
      dhtRows,
      syncRows,
      copyRootId,
      copyShareText,
      dhtMode,
      dhtModeSaving,
      dhtModeMessage,
      onDhtModeChange,
      isMobileLayout,
      emit
    };
  }
});
</script>

<style scoped>
.network-detail-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.network-advanced {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  font-weight: 400;
  color: var(--spark-text-3);
}

.network-simple {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
}

.network-simple .hint {
  margin: 0;
}

.network-simple-text {
  font-size: 13px;
  color: var(--spark-text-2);
}

.network-rows {
  margin-bottom: 16px;
}

.network-dht {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  margin-bottom: 16px;
}

.network-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
}

.network-actions .el-button + .el-button {
  margin-left: 0;
}
</style>
