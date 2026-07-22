<template>
  <section class="mine-page">
    <header v-if="!showRootPage" class="page-header">
      <div class="page-header-main mine-header-main">
        <UserAvatar
          v-if="rootStatus.unlocked"
          :root-id="rootStatus.rootId ?? ''"
          :nickname="rootStatus.nickname ?? ''"
          :avatar="rootStatus.avatar ?? ''"
          :size="48"
        />
        <div>
          <p class="eyebrow">个人中心</p>
          <h1>{{ rootStatus.unlocked ? rootStatus.nickname || '未命名用户' : '我的' }}</h1>
          <p v-if="!rootStatus.initialized || !rootStatus.unlocked" class="lede">
            账号登录前不会显示主界面，先完成 RootID 注册 / 登录。
          </p>
          <p v-else class="lede">RootID 已就绪，可将下方信息发给组织管理员用于添加成员。</p>
        </div>
      </div>
      <div v-if="rootStatus.initialized && rootStatus.unlocked" class="page-header-actions">
        <el-button @click="openProfileEditor">编辑资料</el-button>
        <el-button @click="showRootPage = true">RootID</el-button>
        <el-button type="danger" plain @click="handleLogout">退出登录</el-button>
      </div>
    </header>

    <!-- 编辑资料对话框 -->
    <el-dialog v-model="profileDialogVisible" title="编辑资料" width="420px" append-to-body @closed="resetProfileForm">
      <el-form label-position="top">
        <el-form-item label="头像">
          <AvatarPicker v-model="profileAvatar" :nickname="profileNickname" :disabled="profileSaving" />
          <p class="hint">移除上传图后将恢复为按账号自动生成的配色头像。</p>
        </el-form-item>
        <el-form-item label="昵称">
          <el-input v-model="profileNickname" maxlength="24" placeholder="中英文均可，最长 24 个字符" :disabled="profileSaving" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="profileDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="profileSaving" @click="saveProfile">保存</el-button>
      </template>
    </el-dialog>

    <template v-if="!rootStatus.initialized || !rootStatus.unlocked">
      <RootAuthCenter @open-root-page="showRootPage = true" @update-auth-state="syncAuthState" />
    </template>

    <template v-else>
      <section v-if="!showRootPage" class="content-section">
        <el-card shadow="never" class="panel-card">
          <template #header>
            <h2>节点状态</h2>
          </template>
          <el-descriptions :column="1" border>
            <el-descriptions-item label="RootID">{{ rootStatus.rootId || '未创建' }}</el-descriptions-item>
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

        <el-card shadow="never" class="panel-card">
          <template #header>
            <h2>成员添加资料</h2>
          </template>
          <p class="hint">管理员添加你为成员时需要 RootID 与节点信息。</p>
          <pre class="share-block">{{ shareText }}</pre>
          <div class="row">
            <el-button type="primary" @click="copyShareText">复制资料</el-button>
            <el-button @click="refreshNodeInfo">刷新节点信息</el-button>
          </div>
        </el-card>

        <el-card shadow="never" class="panel-card">
          <template #header>
            <h2>身份备份</h2>
          </template>
          <p class="hint">同一身份的两种备份形式：加密二维码（便捷恢复，可放相册，需配合登录密码）与助记词（离线兜底，最高权限）。</p>
          <el-alert
            v-if="!backupMarked"
            title="尚未完成备份：建议立即保存备份二维码或抄写助记词，避免设备损坏后无法找回账号。"
            type="warning"
            :closable="false"
            show-icon
            class="block-gap"
          />
          <div class="row">
            <el-button :loading="qrLoading" @click="showBackupQr">显示备份二维码</el-button>
            <el-button @click="showRevealDialog = true">查看助记词</el-button>
          </div>

          <el-dialog v-model="showQrDialog" title="备份二维码" width="340px" append-to-body>
            <div class="qr-wrap">
              <img v-if="qrImageUrl" :src="qrImageUrl" alt="备份二维码" class="qr-image" />
            </div>
            <p class="hint">这是同一身份的加密备份，恢复时需输入登录密码。可保存到相册或发送给自己——内容已加密，但请勿与密码存放在一起。</p>
            <div class="row">
              <el-button type="primary" @click="saveQrImage">保存图片</el-button>
            </div>
          </el-dialog>

          <el-dialog v-model="showRevealDialog" title="查看助记词" width="440px" append-to-body @closed="resetReveal">
            <template v-if="!revealedMnemonic">
              <p class="hint">输入登录密码以查看助记词。助记词是账号最高权限，请确认周围无人窥屏。</p>
              <el-input
                v-model="revealPassword"
                type="password"
                show-password
                placeholder="登录密码"
                class="block-gap"
                @keyup.enter="revealMnemonic"
              />
              <div class="row">
                <el-button type="primary" :loading="revealBusy" @click="revealMnemonic">确认查看</el-button>
              </div>
            </template>
            <template v-else>
              <el-alert title="请离线抄写并妥善保存，不要截图、拍照或通过网络发送。" type="warning" :closable="false" show-icon />
              <div class="mnemonic-grid">
                <span v-for="(word, index) in revealedWords" :key="index" class="mnemonic-word">
                  <em>{{ index + 1 }}</em>
                  {{ word }}
                </span>
              </div>
              <div class="row">
                <el-button type="primary" @click="copyRevealedMnemonic">复制助记词</el-button>
              </div>
            </template>
          </el-dialog>
        </el-card>

        <el-card shadow="never" class="panel-card">
          <template #header>
            <h2>数据管理</h2>
          </template>
          <p class="hint">容灾由组织 K 副本网络承担；此处提供用量可见、过期状态清理与手动导出转移。</p>

          <template v-if="dataUsage">
            <el-descriptions :column="1" border class="block-gap">
              <el-descriptions-item
                v-for="row in usageRows"
                :key="row.key"
                :label="row.label"
              >
                {{ row.keys }} 条 · {{ formatBytes(row.bytes) }}
              </el-descriptions-item>
              <el-descriptions-item label="合计">
                {{ dataUsage.totalKeys }} 条 · {{ formatBytes(dataUsage.totalBytes) }}
              </el-descriptions-item>
              <el-descriptions-item v-if="dataUsage.disk" label="磁盘可用">
                {{ formatBytes(dataUsage.disk.freeBytes) }} / {{ formatBytes(dataUsage.disk.totalBytes) }}
                （{{ Math.round(dataUsage.disk.freeRatio * 100) }}%）
              </el-descriptions-item>
            </el-descriptions>

            <el-alert
              v-if="dataUsage.warnings.diskLow"
              title="磁盘可用空间不足：请组织管理员尽快处理——增加磁盘、导出旧数据转移，或在组织详情中执行手动清理。"
              type="error"
              :closable="false"
              show-icon
              class="block-gap"
            />
            <el-alert
              v-else-if="dataUsage.warnings.usageExceeded"
              title="本地数据量较大：建议管理员导出旧数据转移后执行手动清理（组织详情 → 数据治理）。"
              type="warning"
              :closable="false"
              show-icon
              class="block-gap"
            />
          </template>
          <p v-else class="hint block-gap">用量统计加载中…</p>

          <div class="row">
            <el-button :loading="dataActionRunning" @click="runDataCleanup">立即清理</el-button>
            <el-button :loading="dataActionRunning" @click="exportData">导出数据</el-button>
            <el-button @click="refreshDataUsage">刷新用量</el-button>
          </div>
          <el-alert v-if="dataMessage" :title="dataMessage" type="info" :closable="false" show-icon class="block-gap" />
        </el-card>

        <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon />
      </section>

      <RootIDPage v-else @logout="handleLogout" @back="showRootPage = false" />
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import QRCode from 'qrcode';
import type { DataUsageReportDto } from '../../main/preload';
import { formatBytes } from '../utils/format';
import { isIdentityBackupMarked, markIdentityBackupDone } from '../utils/backup-state';
import { errorMessage } from '../utils/ipc';
import UserAvatar from '../components/UserAvatar.vue';
import AvatarPicker from '../components/AvatarPicker.vue';
import RootAuthCenter from './auth/RootAuthCenter.vue';
import RootIDPage from './RootIDPage.vue';

type RootStatus = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
  nickname: string | null;
  avatar: string | null;
};

type P2PInfo = {
  initialized: boolean;
  started: boolean;
  peerId: string | null;
  addresses: string[];
  connectedPeers: string[];
  sparkSyncSubscribers: string[];
  error?: string | null;
};

const USAGE_CLASS_LABELS: Array<{ key: keyof DataUsageReportDto['classes']; label: string }> = [
  { key: 'documents', label: '业务文档' },
  { key: 'indexes', label: '索引' },
  { key: 'syncMeta', label: '同步元数据' },
  { key: 'evidence', label: '存证链' },
  { key: 'organization', label: '组织' },
  { key: 'p2p', label: '网络状态' },
  { key: 'system', label: '系统' },
  { key: 'other', label: '其他' }
];

export default defineComponent({
  name: 'MinePage',
  components: {
    UserAvatar,
    AvatarPicker,
    RootAuthCenter,
    RootIDPage
  },
  setup() {
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });
    const showRootPage = ref(false);
    const message = ref('');
    const p2pInfo = ref<P2PInfo>({ initialized: false, started: false, peerId: null, addresses: [], connectedPeers: [], sparkSyncSubscribers: [], error: null });
    const dataUsage = ref<DataUsageReportDto | null>(null);
    const dataMessage = ref('');
    const dataActionRunning = ref(false);

    const usageRows = computed(() =>
      USAGE_CLASS_LABELS.map((item) => ({
        key: item.key,
        label: item.label,
        keys: dataUsage.value?.classes[item.key]?.keys ?? 0,
        bytes: dataUsage.value?.classes[item.key]?.bytes ?? 0
      }))
    );

    const refreshDataUsage = async () => {
      try {
        dataUsage.value = await window.electronAPI.dataManagement.usage();
      } catch (error) {
        dataMessage.value = `读取用量失败：${error}`;
      }
    };

    const runDataCleanup = async () => {
      dataActionRunning.value = true;
      try {
        const result = await window.electronAPI.dataManagement.cleanupNow();
        dataMessage.value = `清理完成：tombstone ${result.tombstones} 条、节点记录 ${result.peerRecords} 条、同步记账 ${result.orgSyncStates} 条`;
        await refreshDataUsage();
      } catch (error) {
        dataMessage.value = `清理失败：${error}`;
      } finally {
        dataActionRunning.value = false;
      }
    };

    const exportData = async () => {
      dataActionRunning.value = true;
      try {
        const result = await window.electronAPI.dataManagement.exportData();
        dataMessage.value = result.cancelled
          ? '已取消导出'
          : `已导出 ${result.entries} 条数据（${formatBytes(result.bytes)}）到 ${result.path}`;
      } catch (error) {
        dataMessage.value = `导出失败：${error}`;
      } finally {
        dataActionRunning.value = false;
      }
    };

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
    };

    const refreshNodeInfo = async () => {
      try {
        p2pInfo.value = await window.electronAPI.p2p.info();
        if (!p2pInfo.value.started || p2pInfo.value.addresses.length === 0) {
          setTimeout(async () => {
            try {
              p2pInfo.value = await window.electronAPI.p2p.info();
            } catch {
              // Keep current p2p state on retry failure.
            }
          }, 1200);
        }
      } catch (error) {
        message.value = `读取 P2P 信息失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    const shareText = computed(() => {
      const addressesText = p2pInfo.value.addresses.length > 0
        ? p2pInfo.value.addresses.join('\n')
        : '未获取';

      return [
        `RootID: ${rootStatus.value.rootId || '未创建'}`,
        `PeerId: ${p2pInfo.value.peerId || '未获取'}`,
        'P2P Addresses:',
        addressesText
      ].join('\n');
    });

    const copyShareText = async () => {
      try {
        await navigator.clipboard.writeText(shareText.value);
        message.value = '成员添加资料已复制';
        ElMessage.success(message.value);
      } catch (error) {
        message.value = `复制失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    const handleLogout = async () => {
      try {
        await window.electronAPI.rootIdentity.lock();
        showRootPage.value = false;
        message.value = '';
        p2pInfo.value = { initialized: false, started: false, peerId: null, addresses: [], connectedPeers: [], sparkSyncSubscribers: [], error: null };
        await refreshStatus();
        ElMessage.success('已退出登录');
      } catch (error) {
        message.value = `退出失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    // ---------------- 身份备份（加密二维码 + 密码门控助记词） ----------------
    const backupMarked = ref(true);
    const showQrDialog = ref(false);
    const qrImageUrl = ref('');
    const qrLoading = ref(false);
    const showRevealDialog = ref(false);
    const revealPassword = ref('');
    const revealBusy = ref(false);
    const revealedMnemonic = ref('');

    const refreshBackupMarked = () => {
      backupMarked.value = isIdentityBackupMarked(rootStatus.value.rootId);
    };

    const markBackupDone = () => {
      if (rootStatus.value.rootId) {
        markIdentityBackupDone(rootStatus.value.rootId);
        backupMarked.value = true;
      }
    };

    const showBackupQr = async () => {
      qrLoading.value = true;
      try {
        const { payload } = await window.electronAPI.rootIdentity.backupPayload();
        qrImageUrl.value = await QRCode.toDataURL(payload, { errorCorrectionLevel: 'M', margin: 1, width: 280 });
        showQrDialog.value = true;
        markBackupDone();
      } catch (error) {
        ElMessage.error(`生成备份二维码失败：${errorMessage(error)}`);
      } finally {
        qrLoading.value = false;
      }
    };

    const saveQrImage = () => {
      const link = document.createElement('a');
      link.href = qrImageUrl.value;
      link.download = `spark-backup-${(rootStatus.value.rootId ?? 'root').slice(0, 8)}.png`;
      link.click();
    };

    const revealMnemonic = async () => {
      if (!revealPassword.value) {
        ElMessage.warning('请输入登录密码');
        return;
      }
      revealBusy.value = true;
      try {
        const result = await window.electronAPI.rootIdentity.revealMnemonic(revealPassword.value);
        revealedMnemonic.value = result.mnemonic;
        markBackupDone();
      } catch (error) {
        ElMessage.error(`查看失败：${errorMessage(error)}`);
      } finally {
        revealBusy.value = false;
      }
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

    const resetReveal = () => {
      revealPassword.value = '';
      revealedMnemonic.value = '';
    };

    const syncAuthState = (status: RootStatus) => {
      rootStatus.value = status;
      if (!status.unlocked) {
        showRootPage.value = false;
      }
      refreshBackupMarked();
    };

    // ---------------- 资料编辑（昵称 + 头像） ----------------
    const profileDialogVisible = ref(false);
    const profileNickname = ref('');
    const profileAvatar = ref('');
    const profileSaving = ref(false);

    const openProfileEditor = () => {
      profileNickname.value = rootStatus.value.nickname ?? '';
      profileAvatar.value = rootStatus.value.avatar ?? '';
      profileDialogVisible.value = true;
    };

    const resetProfileForm = () => {
      profileNickname.value = '';
      profileAvatar.value = '';
    };

    const saveProfile = async () => {
      if (!profileNickname.value.trim()) {
        ElMessage.warning('昵称不能为空');
        return;
      }
      profileSaving.value = true;
      try {
        const result = await window.electronAPI.rootIdentity.updateProfile({
          nickname: profileNickname.value.trim(),
          avatar: profileAvatar.value || null
        });
        rootStatus.value = { ...rootStatus.value, nickname: result.nickname, avatar: result.avatar };
        profileDialogVisible.value = false;
        ElMessage.success('资料已更新');
      } catch (error) {
        ElMessage.error(`保存失败：${errorMessage(error)}`);
      } finally {
        profileSaving.value = false;
      }
    };

    onMounted(async () => {
      try {
        await refreshStatus();
        refreshBackupMarked();
        await refreshNodeInfo();
        await refreshDataUsage();
      } catch (error) {
        message.value = `读取状态失败：${error}`;
      }
    });

    return {
      rootStatus,
      showRootPage,
      p2pInfo,
      message,
      shareText,
      copyShareText,
      refreshNodeInfo,
      handleLogout,
      syncAuthState,
      dataUsage,
      dataMessage,
      dataActionRunning,
      usageRows,
      formatBytes,
      refreshDataUsage,
      runDataCleanup,
      exportData,
      backupMarked,
      showQrDialog,
      qrImageUrl,
      qrLoading,
      showRevealDialog,
      revealPassword,
      revealBusy,
      revealedMnemonic,
      revealedWords,
      showBackupQr,
      saveQrImage,
      revealMnemonic,
      copyRevealedMnemonic,
      resetReveal,
      profileDialogVisible,
      profileNickname,
      profileAvatar,
      profileSaving,
      openProfileEditor,
      resetProfileForm,
      saveProfile
    };
  }
});
</script>

<style scoped src="../styles/pages/mine.css"></style>
