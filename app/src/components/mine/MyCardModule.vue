<!-- 我的名片模块（MinePage「我的名片」第三、四栏）：
     第三栏=名片方式（二维码名片（缩略图）/名片内容），原「分享链接」占位页已移除；
     第四栏=选中方式的展示 + 操作。
     二维码编码真实节点名片（RootID + p2p.info 的 peerId/监听地址），节点未连接时降级只编码 RootID；
     「名片内容」把加朋友所需的身份 ID 与节点信息整合成文本，一键复制（与二维码名片内容一致，给不方便扫码/传图的场景） -->
<template>
  <!-- 第三栏：名片方式 -->
  <div class="mine-list">
    <h2 class="mine-list-title">我的名片</h2>
    <div class="mine-list-items">
      <button
        type="button"
        class="mine-list-item"
        :class="{ active: activeWay === 'qr' }"
        @click="activeWay = 'qr'"
      >
        <!-- 移动端菜单图标补色（微信式每项一色，同 MinePage 一级菜单色板；桌面端不生效） -->
        <el-icon class="mine-list-item-icon" :size="17" :style="isMobileLayout ? { color: '#34c19b' } : undefined"><Postcard /></el-icon>
        <span class="mine-list-item-text">
          <b>二维码名片</b>
          <span>扫一扫加朋友</span>
        </span>
        <img v-if="qrThumbUrl" :src="qrThumbUrl" alt="名片二维码缩略图" class="card-way-thumb" />
      </button>
      <button
        type="button"
        class="mine-list-item"
        :class="{ active: activeWay === 'id' }"
        @click="activeWay = 'id'"
      >
        <el-icon class="mine-list-item-icon" :size="17" :style="isMobileLayout ? { color: '#3296fa' } : undefined"><Key /></el-icon>
        <span class="mine-list-item-text">
          <b>名片内容</b>
          <span>与二维码相同的信息，一键复制</span>
        </span>
      </button>
    </div>
  </div>

  <!-- 选中方式展示 + 操作：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeWay !== null"
    :title="activeWayLabel"
    @close="activeWay = null"
  >
    <!-- 二维码名片：大图 + 保存/复制 -->
    <el-card v-if="activeWay === 'qr'" shadow="never" class="panel-card">
      <template #header>
        <h2>二维码名片</h2>
      </template>
      <div class="card-show">
        <div class="card-show-qr">
          <img v-if="qrDataUrl" :src="qrDataUrl" alt="名片二维码" class="card-show-qr-img" />
          <span v-else>二维码生成中...</span>
        </div>
        <p class="hint">让对方扫一扫，快速加你为朋友。</p>
        <el-alert
          v-if="!nodeOnline"
          title="节点未连接，名片仅含身份 ID"
          type="warning"
          :closable="false"
          show-icon
        />
        <div class="card-show-actions">
          <el-button type="primary" :disabled="!qrDataUrl" @click="saveQrImage">保存图片</el-button>
          <el-button :disabled="!rootId" @click="copyAll">复制全部信息</el-button>
        </div>
      </div>
    </el-card>

    <!-- 名片内容：加朋友所需的全部信息（与二维码名片同一份），文本形式一键复制 -->
    <el-card v-else shadow="never" class="panel-card">
      <template #header>
        <h2>名片内容</h2>
      </template>
      <p class="hint">
        对方在「通讯录 → 添加朋友」中添加你时，需要你的名片信息（<TermLabel term="rootId" /> 与节点信息），一键复制发给对方即可——与上方二维码名片是同一份内容的文本形式。
      </p>
      <NodeIdentityInfo class="card-identity-rows" :rows="identityRows" />
      <div class="card-show-actions">
        <el-button type="primary" :disabled="!rootId" @click="copyAll">复制全部信息</el-button>
      </div>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Key, Postcard } from '@element-plus/icons-vue';
import QRCode from 'qrcode';
import TermLabel from '../common/TermLabel.vue';
import { isMobileLayout } from '../../stores/ui-layout';
import NodeIdentityInfo, { type NodeIdentityRow } from '../common/NodeIdentityInfo.vue';
import MineDetailContainer from './MineDetailContainer.vue';

type CardWay = 'qr' | 'id';

export default defineComponent({
  name: 'MyCardModule',
  components: { TermLabel, NodeIdentityInfo, MineDetailContainer, Key, Postcard },
  props: {
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    const rootId = ref('');
    const peerId = ref('');
    const addresses = ref<string[]>([]);
    const qrDataUrl = ref('');
    const qrThumbUrl = ref('');
    /** 节点已连接（有 peerId 与可拨号地址）时二维码编码完整节点名片 */
    const nodeOnline = ref(false);
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持默认选中二维码名片
    const activeWay = ref<CardWay | null>(props.detailMode === 'drawer' ? null : 'qr');

    /** 抽屉标题：当前选中名片方式名称 */
    const activeWayLabel = computed(() => {
      const labels: Record<CardWay, string> = { qr: '二维码名片', id: '名片内容' };
      return activeWay.value ? labels[activeWay.value] : '';
    });

    const load = async () => {
      try {
        const status = await window.electronAPI.rootIdentity.status();
        rootId.value = status.rootId ?? '';
      } catch {
        rootId.value = '';
        ElMessage.error('读取身份信息失败');
        return;
      }
      if (!rootId.value) {
        return;
      }
      // 真实节点信息：编码 RootID + peerId + 监听地址；网络未启动时降级只编码 RootID
      let payload = rootId.value;
      try {
        const info = await window.electronAPI.p2p.info();
        peerId.value = info.peerId ?? '';
        addresses.value = info.addresses;
        nodeOnline.value = info.started && !!info.peerId && info.addresses.length > 0;
        if (nodeOnline.value) {
          payload = JSON.stringify({
            type: 'spark-card',
            v: 1,
            rootId: rootId.value,
            peerId: info.peerId,
            addresses: info.addresses
          });
        }
      } catch {
        nodeOnline.value = false;
      }
      qrDataUrl.value = await QRCode.toDataURL(payload, { margin: 1, width: 280 });
      qrThumbUrl.value = await QRCode.toDataURL(payload, { margin: 1, width: 72 });
    };

    onMounted(load);

    // 名片内容：加朋友所需的全部信息
    const identityRows = computed<NodeIdentityRow[]>(() => [
      { label: '身份 ID', term: 'rootId', value: rootId.value, copyable: true, emptyText: '未创建' },
      { label: '节点 ID', term: 'peerId', value: peerId.value, emptyText: '节点未连接' },
      { label: '节点地址', term: 'addresses', value: addresses.value, emptyText: '节点未连接' }
    ]);

    /** 一键复制：身份 ID + 节点 ID + 节点地址 */
    const copyAll = async () => {
      const addressesText = addresses.value.length > 0 ? addresses.value.join('\n') : '未获取';
      const text = [
        `RootID: ${rootId.value || '未创建'}`,
        `PeerId: ${peerId.value || '未获取'}`,
        'P2P Addresses:',
        addressesText
      ].join('\n');
      try {
        await navigator.clipboard.writeText(text);
        ElMessage.success('名片内容已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const saveQrImage = () => {
      const link = document.createElement('a');
      link.href = qrDataUrl.value;
      link.download = `spark-card-${rootId.value.slice(0, 8) || 'root'}.png`;
      link.click();
    };

    return {
      rootId,
      qrDataUrl,
      qrThumbUrl,
      nodeOnline,
      activeWay,
      activeWayLabel,
      identityRows,
      isMobileLayout,
      copyAll,
      saveQrImage
    };
  }
});
</script>

<style scoped>
/* 第三栏二维码缩略图 */
.card-way-thumb {
  flex-shrink: 0;
  width: 36px;
  height: 36px;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-s);
}

/* 第四栏展示区：居中排版 */
.card-show {
  display: flex;
  flex-direction: column;
  gap: 16px;
  align-items: center;
}

.card-show .hint {
  margin: 0;
  text-align: center;
}

.card-show-qr {
  display: flex;
  width: 292px;
  height: 292px;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  color: var(--spark-text-3);
}

.card-show-qr-img {
  width: 280px;
  height: 280px;
}

.card-identity-rows {
  margin: 16px 0;
}

.card-show-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
}

.card-show-actions .el-button + .el-button {
  margin-left: 0;
}
</style>
