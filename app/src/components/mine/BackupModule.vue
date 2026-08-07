<!-- 账号备份模块（MinePage「账号备份」第三、四栏）：
     第三栏=备份方式（二维码备份/助记词备份），原「加密导出」占位页已移除；
     第四栏=对应内容内联展示（不再用弹窗）。敏感内容先验证登录密码再展示——
     二维码备份走 backupPayloadQr（内核验密并产出剔除头像的紧凑载荷），
     助记词备份走 revealMnemonic 验密 -->
<template>
  <!-- 第三栏：备份方式 -->
  <div class="mine-list">
    <h2 class="mine-list-title">账号备份</h2>
    <div class="mine-list-items">
      <button
        v-for="way in ways"
        :key="way.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeWay === way.key }"
        @click="selectWay(way.key)"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="{ color: way.color }"
        ><component :is="way.icon" /></el-icon>
        <span class="mine-list-item-text">
          <b>{{ way.label }}</b>
          <span>{{ way.desc }}</span>
        </span>
      </button>
    </div>
  </div>

  <!-- 详情：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeWay !== null"
    :title="currentWay.label"
    @close="closeDetail"
  >
    <el-card shadow="never" class="panel-card">
      <template #header>
        <h2>{{ currentWay.label }}</h2>
      </template>

      <el-alert
        v-if="!backupMarked"
        title="尚未完成备份：建议立即保存备份二维码或抄写助记词，避免设备损坏后无法找回账号。"
        type="warning"
        :closable="false"
        show-icon
        class="block-gap"
      />

      <!-- 二维码/助记词：密码门控 -->
      <!-- 未验证：密码输入 -->
      <div v-if="!unlocked" class="backup-gate">
        <p class="hint">{{ currentWay.gateHint }}</p>
        <el-input
          v-model="password"
          type="password"
          show-password
          placeholder="登录密码"
          class="backup-gate-input"
          @keyup.enter="verifyAndReveal"
        />
        <div class="backup-actions">
          <el-button type="primary" :loading="busy" @click="verifyAndReveal">验证并显示</el-button>
        </div>
      </div>

      <!-- 已验证：二维码备份 -->
      <div v-else-if="activeWay === 'qr'" class="backup-show">
        <div class="backup-qr" :style="{ width: qrWidth + 12 + 'px', height: qrWidth + 12 + 'px' }">
          <img
            v-if="qrImageUrl"
            :src="qrImageUrl"
            alt="备份二维码"
            class="backup-qr-img"
            :style="{ width: qrWidth + 'px', height: qrWidth + 'px' }"
          />
          <span v-else>二维码生成中...</span>
        </div>
        <el-alert
          v-if="qrDense"
          title="二维码密度较高：扫码时请保证光线充足、摄像头对焦清晰。"
          type="warning"
          :closable="false"
          show-icon
        />
        <p class="hint">
          这是同一身份的加密备份，恢复时需输入登录密码。可保存到相册或发送给自己——内容已加密，但请勿与密码存放在一起。
        </p>
        <div class="backup-actions">
          <el-button type="primary" :disabled="!qrImageUrl" @click="saveQrImage">保存图片</el-button>
          <el-button @click="lockSensitive">隐藏</el-button>
        </div>
      </div>

      <!-- 已验证：助记词备份 -->
      <div v-else class="backup-show">
        <el-alert title="请离线抄写并妥善保存，不要截图、拍照或通过网络发送。" type="warning" :closable="false" show-icon />
        <div class="mnemonic-grid">
          <span v-for="(word, index) in revealedWords" :key="index" class="mnemonic-word">
            <em>{{ index + 1 }}</em>
            {{ word }}
          </span>
        </div>
        <div class="backup-actions">
          <el-button type="primary" @click="copyRevealedMnemonic">复制助记词</el-button>
          <el-button @click="lockSensitive">隐藏</el-button>
        </div>
      </div>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Lock, Postcard } from '@element-plus/icons-vue';
import QRCode from 'qrcode';
import { isIdentityBackupMarked, markIdentityBackupDone } from '../../utils/backup-state';
import { errorMessage } from '../../utils/ipc';
import MineDetailContainer from './MineDetailContainer.vue';

type BackupWayKey = 'qr' | 'mnemonic';

// color 为菜单图标色（微信式每项一色，与 MinePage 一级菜单同规则、同色系色板，移动端与桌面端统一上色）
const WAYS: Array<{ key: BackupWayKey; label: string; desc: string; gateHint: string; icon: Component; color: string }> = [
  {
    key: 'qr',
    label: '二维码备份',
    desc: '加密二维码，便捷恢复',
    gateHint: '输入登录密码以显示备份二维码。二维码内容已加密，恢复时需配合登录密码。',
    icon: Postcard,
    color: '#34c19b'
  },
  {
    key: 'mnemonic',
    label: '助记词备份',
    desc: '离线抄写，最高权限',
    gateHint: '输入登录密码以查看助记词。助记词是账号最高权限，请确认周围无人窥屏。',
    icon: Lock,
    color: '#7b61ff'
  }
];

export default defineComponent({
  name: 'BackupModule',
  components: { MineDetailContainer },
  props: {
    rootId: { type: String as PropType<string | null>, default: null },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持默认选中二维码备份
    const activeWay = ref<BackupWayKey | null>(props.detailMode === 'drawer' ? null : 'qr');
    const backupMarked = ref(true);

    // 密码门控状态（切换备份方式时重置）
    const unlocked = ref(false);
    const password = ref('');
    const busy = ref(false);
    const qrImageUrl = ref('');
    // QR 渲染宽度随载荷长度自适应（密度过高手机难以识别）；qrDense = 高密度提示
    const qrWidth = ref(280);
    const qrDense = ref(false);
    const revealedMnemonic = ref('');

    const currentWay = computed(() => WAYS.find((way) => way.key === activeWay.value) ?? WAYS[0]);

    const refreshBackupMarked = () => {
      backupMarked.value = isIdentityBackupMarked(props.rootId);
    };

    const markBackupDone = () => {
      if (props.rootId) {
        markIdentityBackupDone(props.rootId);
        backupMarked.value = true;
      }
    };

    /** 切换备份方式：清空敏感内容与密码，回到密码门控 */
    const lockSensitive = () => {
      unlocked.value = false;
      password.value = '';
      qrImageUrl.value = '';
      qrWidth.value = 280;
      qrDense.value = false;
      revealedMnemonic.value = '';
    };

    const selectWay = (way: BackupWayKey) => {
      if (way !== activeWay.value) {
        lockSensitive();
      }
      activeWay.value = way;
    };

    /** 关闭详情（drawer 模式）：清空选中与敏感内容，抽屉随之关闭 */
    const closeDetail = () => {
      lockSensitive();
      activeWay.value = null;
    };

    /** 验密并展示敏感内容（二维码走 backupPayloadQr 自带验密；助记词走 revealMnemonic 验密） */
    const verifyAndReveal = async () => {
      if (!password.value) {
        ElMessage.warning('请输入登录密码');
        return;
      }
      busy.value = true;
      try {
        if (activeWay.value === 'qr') {
          // 二维码备份载荷已剔除头像等大字段（适配 QR 容量），密码错误时内核报错
          const { payload } = await window.electronAPI.rootIdentity.backupPayloadQr(password.value);
          // 渲染宽度按载荷长度自适应：越长越密，需更大尺寸保证可扫
          qrWidth.value = payload.length < 800 ? 280 : payload.length < 1600 ? 400 : 520;
          qrDense.value = payload.length >= 1600;
          qrImageUrl.value = await QRCode.toDataURL(payload, {
            errorCorrectionLevel: 'M',
            margin: 1,
            width: qrWidth.value
          });
        } else {
          const { mnemonic } = await window.electronAPI.rootIdentity.revealMnemonic(password.value);
          revealedMnemonic.value = mnemonic;
        }
        unlocked.value = true;
        markBackupDone();
      } catch (error) {
        ElMessage.error(`验证失败：${errorMessage(error)}`);
      } finally {
        busy.value = false;
      }
    };

    const saveQrImage = () => {
      const link = document.createElement('a');
      link.href = qrImageUrl.value;
      link.download = `spark-backup-${(props.rootId ?? 'root').slice(0, 8)}.png`;
      link.click();
    };

    const revealedWords = computed(() => (revealedMnemonic.value ? revealedMnemonic.value.split(' ') : []));

    const copyRevealedMnemonic = async () => {
      try {
        await navigator.clipboard.writeText(revealedMnemonic.value);
        ElMessage.success('已复制助记词');
      } catch {
        ElMessage.warning('复制失败，请手动抄写');
      }
    };

    // 等价于原 BackupCard 的 rootId watch：备份完成标记随身份变化自动刷新
    watch(() => props.rootId, refreshBackupMarked, { immediate: true });

    return {
      ways: WAYS,
      activeWay,
      currentWay,
      backupMarked,
      unlocked,
      password,
      busy,
      qrImageUrl,
      qrWidth,
      qrDense,
      revealedWords,
      selectWay,
      closeDetail,
      verifyAndReveal,
      lockSensitive,
      saveQrImage,
      copyRevealedMnemonic
    };
  }
});
</script>

<style scoped>
.backup-gate {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-top: 12px;
}

.backup-gate .hint {
  margin: 0;
}

.backup-gate-input {
  max-width: 280px;
}

.backup-show {
  display: flex;
  flex-direction: column;
  gap: 16px;
  margin-top: 12px;
}

.backup-show .hint {
  margin: 0;
}

.backup-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
}

.backup-actions .el-button + .el-button {
  margin-left: 0;
}

.backup-qr {
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  color: var(--spark-text-3);
}

.backup-qr-img {
  image-rendering: pixelated;
}

.mnemonic-grid {
  display: grid;
  grid-template-columns: repeat(6, 1fr);
  gap: 8px;
}

.mnemonic-word {
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  padding: 6px 0;
  text-align: center;
  font-size: 16px;
  background: var(--spark-bg-hover);
}

.mnemonic-word em {
  display: block;
  font-style: normal;
  font-size: 10px;
  color: var(--spark-text-3);
}
</style>
