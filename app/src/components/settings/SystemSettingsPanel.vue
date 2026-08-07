<!-- 设置页「系统设置」：第三栏子菜单 + 第四栏内容（网络状态 / 设备管理 / 存储 / 通用 / 通知 / 关于）。
     网络状态、设备管理复用个人设置的模块（自带列表栏，编辑走抽屉）；
     账号与安全已移除（资料修改在「我的资料」、账号备份在「账号备份」模块），隐私组已删除（朋友权限模块覆盖） -->
<template>
  <div class="system-settings-panel">
  <!-- 第三栏：子菜单（移动端选中 section 时隐藏，内容整页覆盖） -->
  <div class="mine-list panel-submenu" v-if="activeSection === null || !isMobileLayout">
    <h2 class="mine-list-title">系统设置</h2>
    <div class="mine-list-items">
      <button
        v-for="item in sections"
        :key="item.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeSection === item.key }"
        @click="activeSection = item.key"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="isMobileLayout ? { color: item.color } : undefined"
        ><component :is="item.icon" /></el-icon>
        <b class="settings-section-label">{{ item.label }}</b>
      </button>
    </div>
  </div>

  <!-- 移动端内容整页返回栏（Android 前端改造）：选中 section 时显示，返回回子菜单；
       Transition 进出自右滑入/向右滑出（与导航栈 push/pop 同向） -->
  <Transition name="settings-overlay-slide">
  <div v-if="isMobileLayout && activeSection !== null" class="settings-mobile-bar">
    <MobileBackBar :title="activeSectionLabel" @back="activeSection = null" />
  </div>
  </Transition>

  <!-- 网络状态 / 设备管理：模块直接渲染（列表栏即第四栏，编辑页走抽屉）；
       Transition 包裹使移动端内容整页覆盖层进出自右滑入、返回向右滑出（桌面端无对应样式，无动画） -->
  <Transition name="settings-overlay-slide">
  <NetworkModule
    v-if="activeSection === 'netStatus'"
    detail-mode="drawer"
    :root-id="rootStatus.rootId ?? ''"
    :p2p-info="p2pInfo"
    show-proxy
    @refresh="refreshNodeInfo"
  />
  <DevicesModule
    v-else-if="activeSection === 'devices'"
    detail-mode="drawer"
    :root-id="rootStatus.rootId ?? ''"
    :p2p-info="p2pInfo"
  />

  <!-- 其余子项：第四栏内容（移动端未选 section 时不渲染——否则空白覆盖层会挡住子菜单） -->
  <div v-else-if="activeSection !== null || !isMobileLayout" class="mine-detail">
    <!-- 存储：本地用量 + 清理 + 导出（dataManagement 真实接口） -->
    <el-card v-if="activeSection === 'storage'" shadow="never" class="panel-card">
      <template #header>
        <h2>存储管理</h2>
      </template>
      <p class="hint">本地数据用量、过期状态清理与手动导出转移。</p>

      <template v-if="dataUsage">
        <el-descriptions :column="1" border class="block-gap">
          <el-descriptions-item v-for="row in usageRows" :key="row.key" :label="row.label">
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
          title="磁盘可用空间不足：请尽快处理——增加磁盘、导出旧数据转移，或执行手动清理。"
          type="error"
          :closable="false"
          show-icon
          class="block-gap"
        />
        <el-alert
          v-else-if="dataUsage.warnings.usageExceeded"
          title="本地数据量较大：建议导出旧数据转移后执行手动清理。"
          type="warning"
          :closable="false"
          show-icon
          class="block-gap"
        />
      </template>
      <p v-else class="hint block-gap">用量统计加载中…</p>

      <div class="settings-actions">
        <el-button :loading="dataActionRunning" @click="runDataCleanup">立即清理</el-button>
        <el-button :loading="dataActionRunning" @click="exportData">导出数据</el-button>
        <el-button @click="refreshDataUsage">刷新用量</el-button>
      </div>
      <el-alert v-if="dataMessage" :title="dataMessage" type="info" :closable="false" show-icon class="block-gap" />
    </el-card>

    <!-- 通用：外观主题（真实生效，stores/theme）+ 其余偏好开关（mock） -->
    <el-card v-else-if="activeSection === 'general'" shadow="never" class="panel-card">
      <template #header>
        <h2>通用设置</h2>
      </template>
      <div class="settings-rows">
        <div class="settings-row">
          <span>外观</span>
          <el-radio-group v-model="themeMode" size="small">
            <el-radio-button value="system">跟随系统</el-radio-button>
            <el-radio-button value="light">浅色</el-radio-button>
            <el-radio-button value="dark">深色</el-radio-button>
          </el-radio-group>
        </div>
        <!-- TODO(mock): 以下开关仅本地展示不生效，待偏好持久化方案落地 -->
        <div v-for="item in generalItems" :key="item.key" class="settings-row">
          <span>{{ item.label }}</span>
          <el-switch v-model="generalStates[item.key]" />
        </div>
      </div>
    </el-card>
    <MockSettingGroup v-else-if="activeSection === 'notify'" title="消息通知" :items="notifyItems" hint="消息声音、桌面通知与免打扰。" />

    <!-- 关于 -->
    <el-card v-else shadow="never" class="panel-card">
      <template #header>
        <h2>关于</h2>
      </template>
      <el-descriptions :column="1" border>
        <el-descriptions-item label="版本">{{ appVersion }}</el-descriptions-item>
        <el-descriptions-item label="产品">Spark 桌面端 · 去中心化的组织协作网络</el-descriptions-item>
        <el-descriptions-item label="许可证">见仓库根目录 LICENSE</el-descriptions-item>
      </el-descriptions>
      <!-- 手动更新（自动检查下载就绪后用户取消了弹窗时的入口；
           无桥接/未配置更新源时按钮禁用） -->
      <div class="about-update">
        <el-button size="small" :loading="updateChecking" :disabled="!updaterAvailable" @click="checkUpdate">
          检查更新
        </el-button>
        <el-button
          v-if="updateStaged"
          size="small"
          type="primary"
          :loading="updateApplying"
          @click="applyUpdate"
        >
          重启安装 {{ updateStaged.version }}
        </el-button>
        <span v-if="updateMessage" class="about-update-message">{{ updateMessage }}</span>
      </div>
    </el-card>
  </div>
  </Transition>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onBeforeUnmount, onMounted, ref, watch, type Component } from 'vue';
import { Bell, Coin, Connection, InfoFilled, Monitor, SetUp } from '@element-plus/icons-vue';
import type { DataUsageReportDto, P2pInfoDto as P2PInfo } from '../../api';
import { formatBytes } from '../../utils/format';
import { themeMode } from '../../stores/theme';
import { isMobileLayout } from '../../stores/ui-layout';
import { isOverlayCloseTarget, popOverlay, pushOverlay } from '../../stores/overlay-stack';
import MobileBackBar from '../MobileBackBar.vue';
import NetworkModule from '../mine/NetworkModule.vue';
import DevicesModule from '../mine/DevicesModule.vue';
import MockSettingGroup, { type MockSettingItem } from './MockSettingGroup.vue';

type RootStatus = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
  nickname: string | null;
  avatar: string | null;
};

type SectionKey = 'netStatus' | 'devices' | 'storage' | 'general' | 'notify' | 'about';

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

// TODO(mock): 以下开关仅本地展示不生效，待对应设置项的后端/持久化方案落地
const GENERAL_ITEMS: MockSettingItem[] = [
  { key: 'largeFont', label: '大字体' },
  { key: 'shortcutHints', label: '快捷键提示' }
];

// TODO(mock): 同上（震动提醒/桌面角标自原个人空间「通知」设置并入）
const NOTIFY_ITEMS: MockSettingItem[] = [
  { key: 'sound', label: '新消息声音' },
  { key: 'vibrate', label: '震动提醒' },
  { key: 'desktop', label: '桌面通知' },
  { key: 'badge', label: '桌面角标' },
  { key: 'dnd', label: '免打扰时段' }
];

export default defineComponent({
  name: 'SystemSettingsPanel',
  components: {
    NetworkModule,
    DevicesModule,
    MockSettingGroup,
    MobileBackBar
  },
  props: {
    /** 初始定位的子菜单（深链入口，如移动端网络状态点跳「网络状态」）；仅在挂载时生效一次 */
    initialSection: { type: String as () => SectionKey, default: undefined }
  },
  setup(props) {
    // 移动端（Android 前端改造）：子菜单整页 <-> 内容整页覆盖层；桌面端保持「子菜单+内容」分栏
    const activeSection = ref<SectionKey | null>(props.initialSection ?? (isMobileLayout.value ? null : 'general'));
    // 覆盖层登记（token 制）：移动端选中 section（内容整页）时入栈，返回子菜单/卸载时出栈；
    // 系统回退键仅关栈顶（本面板内容页上还可能叠着模块详情整页，须逐层回退）
    let overlayToken: symbol | null = null;
    const releaseOverlay = () => {
      popOverlay(overlayToken);
      overlayToken = null;
    };
    watch(
      () => activeSection.value !== null && isMobileLayout.value,
      (opened) => {
        // 页级返回栏不再随 section 开关隐藏（覆盖层以页面为定位基准整页盖住它），
        // 此处只维护覆盖层登记
        if (opened && !overlayToken) {
          overlayToken = pushOverlay();
        } else if (!opened) {
          releaseOverlay();
        }
      },
      { immediate: true }
    );
    onBeforeUnmount(releaseOverlay);
    const onCloseOverlay = (event: Event) => {
      if (isOverlayCloseTarget(event, overlayToken) && activeSection.value !== null && isMobileLayout.value) {
        activeSection.value = null;
      }
    };
    onMounted(() => window.addEventListener('spark:close-overlay', onCloseOverlay));
    onBeforeUnmount(() => window.removeEventListener('spark:close-overlay', onCloseOverlay));
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null, nickname: null, avatar: null });
    const p2pInfo = ref<P2PInfo>({
      initialized: false,
      started: false,
      peerId: null,
      addresses: [],
      connectedPeers: [],
      sparkSyncSubscribers: [],
      error: null
    });
    const dataUsage = ref<DataUsageReportDto | null>(null);
    const dataMessage = ref('');
    const dataActionRunning = ref(false);
    const generalStates = ref<Record<string, boolean>>({});

    // 顺序按用户习惯：通用偏好在前，设备/网络/存储等系统项居中，关于垫底；
    // color 为移动端菜单图标色（微信式每项一色，取色与 utils/palette 品牌色板同源，桌面端不使用）
    const sections: Array<{ key: SectionKey; label: string; icon: Component; color: string }> = [
      { key: 'general', label: '通用设置', icon: SetUp, color: '#64748b' },
      { key: 'notify', label: '消息通知', icon: Bell, color: '#eb2f96' },
      { key: 'netStatus', label: '网络状态', icon: Connection, color: '#00b8a9' },
      { key: 'devices', label: '设备管理', icon: Monitor, color: '#3296fa' },
      { key: 'storage', label: '存储管理', icon: Coin, color: '#f7b500' },
      { key: 'about', label: '关于', icon: InfoFilled, color: '#94a3b8' }
    ];

    /** 当前选中子菜单的标题（移动端整页返回栏标题） */
    const activeSectionLabel = computed(
      () => sections.find((sec) => sec.key === activeSection.value)?.label ?? ''
    );

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

    /** 节点信息刷新（网络状态模块的 refresh 事件） */
    const refreshNodeInfo = async () => {
      try {
        p2pInfo.value = await window.electronAPI.p2p.info();
      } catch {
        // 读取失败保留当前状态
      }
    };

    // ------------------------------------------------------------------
    // 关于·手动更新（自动检查就绪弹窗被取消后的入口；命令语义见
    // src-tauri commands/updater.rs。无桥接/未配置时按钮禁用）
    // ------------------------------------------------------------------
    const appVersion = ref('0.2.1');
    const updaterAvailable = ref(false);
    const updateChecking = ref(false);
    const updateApplying = ref(false);
    const updateMessage = ref('');
    const updateStaged = ref<{ fileName: string; version: string } | null>(null);

    const refreshUpdater = async () => {
      const updater = window.electronAPI?.updater;
      if (!updater) {
        return;
      }
      try {
        const status = await updater.status();
        appVersion.value = status.currentVersion || appVersion.value;
        updaterAvailable.value = status.configured;
        updateStaged.value = status.staged ?? null;
        if (status.staged) {
          updateMessage.value = `新版本 ${status.staged.version} 已就绪，点击「重启安装」完成更新`;
        }
      } catch {
        // 读取失败保留默认展示（按钮保持禁用）
      }
    };

    /** 检查更新：有更新时直接下载（就绪后出「重启安装」按钮，同 TestPage 调试面板流程） */
    const checkUpdate = async () => {
      const updater = window.electronAPI?.updater;
      if (!updater) {
        return;
      }
      updateChecking.value = true;
      updateMessage.value = '';
      try {
        const result = await updater.check();
        if (!result.updateAvailable) {
          updateMessage.value = '当前已是最新版本';
          return;
        }
        updateMessage.value = `发现新版本 ${result.availableVersion}，正在下载…`;
        const staged = await updater.stageLatest();
        updateStaged.value = staged;
        updateMessage.value = `新版本 ${staged.version} 已就绪，点击「重启安装」完成更新`;
      } catch (error) {
        updateMessage.value = `检查或下载更新失败：${error}`;
      } finally {
        updateChecking.value = false;
      }
    };

    const applyUpdate = async () => {
      const updater = window.electronAPI?.updater;
      if (!updater) {
        return;
      }
      updateApplying.value = true;
      try {
        await updater.applyRestart();
        // 成功路径应用随即重启；失败才走到下面
      } catch (error) {
        updateMessage.value = `应用更新失败：${error}`;
        updateApplying.value = false;
      }
    };

    onMounted(async () => {
      try {
        rootStatus.value = await window.electronAPI.rootIdentity.status();
      } catch {
        // 状态读取失败保留默认展示
      }
      await refreshNodeInfo();
      await refreshDataUsage();
      await refreshUpdater();
    });

    return {
      activeSection,
      activeSectionLabel,
      isMobileLayout,
      sections,
      rootStatus,
      p2pInfo,
      dataUsage,
      dataMessage,
      dataActionRunning,
      usageRows,
      formatBytes,
      refreshNodeInfo,
      refreshDataUsage,
      runDataCleanup,
      exportData,
      themeMode,
      generalStates,
      generalItems: GENERAL_ITEMS,
      notifyItems: NOTIFY_ITEMS,
      appVersion,
      updaterAvailable,
      updateChecking,
      updateApplying,
      updateMessage,
      updateStaged,
      checkUpdate,
      applyUpdate
    };
  }
});
</script>

<style scoped>
/* 子菜单项标签：常规字重，选中行加粗（同 ProfileModule 字段列表） */
.settings-section-label {
  font-size: 14px;
  font-weight: 400;
}

.mine-list-item.active .settings-section-label {
  font-weight: 600;
}

/* 通用设置行：左标签右控件（外观单选 / 偏好开关） */
.settings-rows {
  display: flex;
  flex-direction: column;
}

.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 0;
  font-size: 13px;
  color: var(--spark-text-1);
}

.settings-row + .settings-row {
  border-top: 1px solid var(--spark-border-light);
}

/* 关于·手动更新行：按钮组 + 状态文案（弱化的次要信息） */
.about-update {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 14px;
}

.about-update-message {
  font-size: 12px;
  color: var(--spark-text-2, #909399);
}

/* ---- 移动端（≤768px，Android 前端改造）：子菜单整页 + 内容整页覆盖 ----
   选中 section 时内容区 absolute 覆盖整页（顶部让出返回栏高度），不上下分屏 */
@media (max-width: 768px) {
  .system-settings-panel {
    /* 移动端面板自身不作定位包含块（保持 static）：内容覆盖层/返回栏的 absolute 一路向上
       解析到页面 section（.mine-page，position:relative）。返回关闭 section 时页级返回栏复现、
       面板随 flex 下移，若覆盖层以面板为基准，离场层起始位置会偏下一个返回栏高度（Android 前端改造） */
    flex: 1;
    min-height: 0;
    width: 100%;
  }

  /* 子菜单（直接子级 mine-list）整页占满 */
  .system-settings-panel > .mine-list.panel-submenu {
    flex: 1;
    height: 100%;
  }

  /* 内容区：NetworkModule/DevicesModule 内部 mine-list 或 .mine-detail，absolute 整页覆盖；
     定位基准为页面 section（面板保持 static，见上），top 让出返回栏高度（48px + 状态栏安全区），
     避免首行内容被返回栏遮挡；
     :not(.panel-submenu) 排除面板自身子菜单（其与内容区互斥渲染，但需防 CSS 命中）；
     进出动画由 Transition（settings-overlay-slide）承担 */
  .system-settings-panel > :deep(.mine-list):not(.panel-submenu),
  .system-settings-panel > .mine-detail {
    position: absolute;
    top: calc(48px + env(safe-area-inset-top, 0px));
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 2;
    background: var(--spark-bg-card);
    overflow-y: auto;
  }

  /* 返回栏：浮在面板顶部（与内容同向滑动，视觉等同整页推入/推出） */
  .settings-mobile-bar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 3;
  }

  /* 覆盖层进出动画（与导航栈 push/pop 同款曲线与方向：进=自右滑入，出=向右滑出） */
  .settings-overlay-slide-enter-active,
  .settings-overlay-slide-leave-active {
    transition: transform 260ms cubic-bezier(0.25, 0.46, 0.45, 0.94);
    will-change: transform;
  }

  .settings-overlay-slide-enter-from,
  .settings-overlay-slide-leave-to {
    transform: translateX(100%);
  }

  /* 移动端子菜单字号按微信调大：主文字 16px；移动端菜单是页面切换不是选中，选中态不生效 */
  .settings-section-label {
    font-size: 16px;
  }

  .mine-list-item.active .settings-section-label {
    font-weight: 400;
  }
}
</style>
