<!--
  公告通知插件（spark-announcement）· 主视图（app 视图）：公告列表 → 公告详情
  两层导航 + 发布者视角的起草/撤回入口（非发布权集合成员不渲染入口）。

  沿用 spark-forum ForumView 的头部模式与机制：
  - runtime.currentRoot / listMineOrganizations / syncOrganizationData
    （org:read、org:sync）；
  - docs 读写全部经 service 层（storage:read/write）；
  - identity:sign 发布/撤回防抵赖 + identity.verify 免权限验签（「已签名」徽标）；
  - messages.sendAppMessage 公告卡片（message:app，发布者本机即时反馈）；
    成员侧卡片在同步后由本机插件实例「本地生成」（§20.4.3，插件加载时补发 +
    节流，档二-4 MVP 降级口径）；
  - messages.onCardAction 接收公告卡片「查看全文」回调 → 切组织、定位公告；
  - 撤回 = 追加撤回记录（append-only 不回滚已送达），提交时如实告知不可收回；
  - 档二-10：组织预置插件清单未落地，以安装引导文案缓解（公告卡片只推送到
    已装本插件的成员设备）。
-->
<template>
  <section class="spark-announcement">
    <el-alert
      v-if="message"
      :title="message"
      :type="messageType"
      :closable="false"
      show-icon
      class="message"
    />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">公告通知</p>
          <h2>组织公告栏</h2>
          <p class="lede">版本公告与团队通知：公告数据经组织同步，各成员本机实例生成为应用会话消息卡片。</p>
        </div>
        <el-button @click="reloadAll" :loading="loading">刷新</el-button>
      </div>

      <el-form label-position="top" class="selectors" v-if="orgOptions.length > 0">
        <el-form-item label="组织">
          <el-select v-model="selectedOrgId" @change="onOrgChange" placeholder="选择组织">
            <el-option
              v-for="org in orgOptions"
              :key="org.orgId"
              :label="`${org.name} (${org.orgId.slice(0, 8)}...)`"
              :value="org.orgId"
            />
          </el-select>
        </el-form-item>
      </el-form>

      <el-empty v-if="orgOptions.length === 0" description="你还没有加入任何组织。" />

      <div v-if="activeOrg" class="meta-row">
        <el-tag type="info" class="root-id-tag">
          <span class="root-id-text">当前 RootID: {{ currentRootId || '-' }}</span>
        </el-tag>
        <el-tag :type="isAdmin ? 'danger' : 'warning'">
          {{ isAdmin ? '组织管理员' : '组织成员' }}
        </el-tag>
        <el-tag v-if="canPublish" type="success">发布权集合成员</el-tag>
      </div>
    </el-card>

    <!-- 档二-10 安装引导：全员可达依赖成员普遍安装本插件 -->
    <el-alert
      v-if="activeOrg"
      type="info"
      :closable="false"
      show-icon
      title="公告卡片只会推送到已安装本插件的成员设备（同步的是公告数据，卡片由各成员本机生成）。建议组织成员都安装「公告通知」插件。"
    />

    <!-- 发布权配置未初始化：任何成员都无发布路径（fail-closed），引导管理员初始化 -->
    <el-alert
      v-if="activeOrg && !config"
      type="warning"
      :closable="false"
      show-icon
      title="发布权配置尚未初始化：暂无人可发布公告。请组织管理员在「发布权配置」中登记发布者。"
    />

    <!-- 公告列表页 -->
    <el-card v-if="activeOrg && view === 'list'" shadow="never">
      <template #header>
        <div class="header-row">
          <div class="header-row-inner">
            <h3>公告</h3>
            <el-radio-group v-model="kindFilter" size="small">
              <el-radio-button value="all">全部</el-radio-button>
              <el-radio-button value="release">版本公告</el-radio-button>
              <el-radio-button value="notice">团队通知</el-radio-button>
            </el-radio-group>
          </div>
          <div class="header-row-inner">
            <el-button v-if="isAdmin" size="small" @click="openConfigDialog">发布权配置</el-button>
            <el-button v-if="canPublish" type="primary" size="small" @click="openPublishDialog">发布公告</el-button>
          </div>
        </div>
      </template>

      <el-empty v-if="listItems.length === 0" description="暂无公告" />

      <div
        v-for="item in listItems"
        :key="item.announcement.id"
        :id="`ann-${item.announcement.id}`"
        class="ann-item"
        :class="{ retracted: !!item.retraction, highlighted: highlightedId === item.announcement.id }"
        @click="enterDetail(item.announcement.id)"
      >
        <div class="ann-title-row">
          <el-tag :type="item.announcement.kind === 'release' ? 'warning' : 'success'" size="small">
            {{ item.announcement.kind === 'release' ? '版本公告' : '团队通知' }}
          </el-tag>
          <el-tag v-if="item.announcement.version" size="small" type="info">{{ item.announcement.version }}</el-tag>
          <el-tag v-if="item.retraction" size="small" type="danger">已撤回</el-tag>
          <strong class="ann-title">{{ item.announcement.title }}</strong>
        </div>
        <p class="ann-preview">{{ bodyPreview(item.announcement.body) }}</p>
        <div class="post-meta">
          <span class="author">{{ item.announcement.publisherRootId }}</span>
          <span>{{ formatDate(item.announcement.publishedAt) }}</span>
        </div>
      </div>
    </el-card>

    <!-- 公告详情页 -->
    <el-card
      v-if="activeOrg && view === 'detail' && activeAnnouncement"
      shadow="never"
      :id="`ann-detail-${activeAnnouncement.id}`"
      :class="{ 'detail-highlighted': highlightedId === activeAnnouncement.id }"
    >
      <template #header>
        <div class="header-row">
          <div class="header-row-inner">
            <el-button size="small" text @click="view = 'list'">← 公告列表</el-button>
            <h3 class="ann-detail-title">{{ activeAnnouncement.title }}</h3>
          </div>
        </div>
      </template>

      <div class="ann-state-row">
        <el-tag :type="activeAnnouncement.kind === 'release' ? 'warning' : 'success'" size="small">
          {{ activeAnnouncement.kind === 'release' ? '版本公告' : '团队通知' }}
        </el-tag>
        <el-tag v-if="activeAnnouncement.version" size="small" type="info">{{ activeAnnouncement.version }}</el-tag>
        <el-tag v-if="activeRetraction" size="small" type="danger">已撤回</el-tag>
        <el-tag v-if="activeAnnouncement.signature" type="success" size="small">已签名</el-tag>
        <el-button
          v-if="activeAnnouncement.signature"
          size="small"
          text
          :loading="verifying"
          @click="verifyActiveAnnouncement"
        >
          验签
        </el-button>
        <span v-if="verifyResult" class="verify-result">{{ verifyResult }}</span>
      </div>

      <!-- 撤回诚实呈现：不删除痕迹，原公告与撤回记录均可查 -->
      <el-alert
        v-if="activeRetraction"
        type="error"
        :closable="false"
        show-icon
        class="retraction-alert"
        :title="`本公告已于 ${formatDate(activeRetraction.retractedAt)} 撤回${activeRetraction.reason ? `：${activeRetraction.reason}` : '。'}`"
        description="撤回为追加记录，不删除原公告；已送达的消息卡片不回滚，仅标注「已撤回」。"
      />
      <!-- 撤回验签入口（免权限）：撤回记录带签名时可校验「确为撤回人域身份签发」 -->
      <div v-if="activeRetraction?.signature" class="retraction-verify-row">
        <el-tag type="success" size="small">撤回已签名</el-tag>
        <el-button size="small" text :loading="retractionVerifying" @click="verifyActiveRetraction">
          撤回验签
        </el-button>
        <span v-if="retractionVerifyResult" class="verify-result">{{ retractionVerifyResult }}</span>
      </div>

      <div class="post-meta">
        <span class="author">发布者：{{ activeAnnouncement.publisherRootId }}</span>
        <span>{{ formatDate(activeAnnouncement.publishedAt) }}</span>
      </div>

      <p v-if="activeAnnouncement.releaseRef" class="release-ref">
        发布记录引用：{{ activeAnnouncement.releaseRef }}
        <span class="hint-inline">（展示级自由字段，契约落地后升级为类型化引用）</span>
      </p>

      <p class="ann-content">{{ activeAnnouncement.body }}</p>

      <!-- 签名信息可查（诚实呈现：不做「已验证」之外的美化） -->
      <div v-if="activeAnnouncement.signature" class="signature-info">
        <el-divider content-position="left">签名信息</el-divider>
        <p>公钥：{{ activeAnnouncement.signature.publicKey }}</p>
        <p>载荷哈希（payload 前缀）：{{ activeAnnouncement.signature.payload.slice(0, 64) }}…</p>
      </div>

      <div class="ann-ops">
        <el-button v-if="canRetractActive && !activeRetraction" size="small" type="danger" @click="openRetractDialog">
          撤回公告
        </el-button>
      </div>
    </el-card>

    <!-- 发布公告对话框 -->
    <el-dialog v-model="publishDialogVisible" title="发布公告" width="560px">
      <el-form label-position="top">
        <el-form-item label="类型">
          <el-radio-group v-model="publishDraft.kind">
            <el-radio-button value="release" :disabled="config?.enableRelease === false">版本公告</el-radio-button>
            <el-radio-button value="notice" :disabled="config?.enableNotice === false">团队通知</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <template v-if="publishDraft.kind === 'release'">
          <el-form-item label="版本号（可选，展示级自由字段）">
            <el-input v-model="publishDraft.version" :maxlength="40" show-word-limit placeholder="如 v0.2.0" />
          </el-form-item>
          <el-form-item label="发布记录引用（可选，发布单集合记录 id）">
            <el-input v-model="publishDraft.releaseRef" :maxlength="120" show-word-limit placeholder="release_..." />
          </el-form-item>
        </template>
        <el-form-item label="标题">
          <el-input v-model="publishDraft.title" :maxlength="120" show-word-limit placeholder="一句话说清公告主题" />
        </el-form-item>
        <el-form-item label="正文（纯文本）">
          <el-input v-model="publishDraft.body" type="textarea" :rows="8" :maxlength="20000" show-word-limit />
        </el-form-item>
      </el-form>
      <p class="hint">
        公告数据仅追加、不覆盖：发布后不可编辑，纠错请发更正公告或撤回（撤回同样只追加记录，已送达卡片不回滚）。
        发布将请求一次域身份签名（防抵赖），拒绝签名也会照发（少「已签名」徽标）。
        版本卡片在 MVP 期由发布管理件唯一推送，此处发布的版本公告为手动公告。
      </p>
      <template #footer>
        <el-button @click="publishDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="publishing" @click="submitPublish">发布公告</el-button>
      </template>
    </el-dialog>

    <!-- 撤回对话框 -->
    <el-dialog v-model="retractDialogVisible" title="撤回公告" width="420px">
      <p v-if="activeAnnouncement" class="retract-target">
        撤回对象：<strong>{{ activeAnnouncement.title }}</strong>
      </p>
      <el-alert
        type="warning"
        :closable="false"
        show-icon
        title="撤回以追加记录形式永久留痕、不可收回：原公告不删除，已送达的消息卡片不回滚，仅标注「已撤回」。"
      />
      <el-form label-position="top" class="retract-form">
        <el-form-item label="撤回理由（可选，随记录留痕）">
          <el-input v-model="retractReason" type="textarea" :rows="2" :maxlength="200" show-word-limit />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="retractDialogVisible = false">取消</el-button>
        <el-button type="danger" :loading="retracting" @click="submitRetract">确认撤回</el-button>
      </template>
    </el-dialog>

    <!-- 发布权配置对话框（档三-23：MVP 名册管理员直改） -->
    <el-dialog v-model="configDialogVisible" title="发布权配置" width="520px">
      <el-form label-position="top">
        <el-form-item label="发布者 RootID 集合（每行一个；内核不认识发布权，本集合是业务层校验依据）">
          <el-input
            v-model="configDraft.publisherText"
            type="textarea"
            :rows="5"
            placeholder="root_id_1&#10;root_id_2"
          />
        </el-form-item>
        <el-form-item label="类型开关">
          <el-checkbox v-model="configDraft.enableRelease">启用版本公告</el-checkbox>
          <el-checkbox v-model="configDraft.enableNotice">启用团队通知</el-checkbox>
        </el-form-item>
      </el-form>
      <p class="hint">MVP 期由组织管理员直改初始化；后续变更将挂组织治理事务（「规则挂事务」迭代）。</p>
      <template #footer>
        <el-button @click="configDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="configSaving" @click="submitConfig">保存</el-button>
      </template>
    </el-dialog>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginCardActionPayload, PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildAnnouncementList,
  canManageAnnounceConfig,
  canPublishAnnouncement,
  canRetractAnnouncement,
  deriveRetractionMap,
  findApplicableRetraction,
  validateAnnouncementBody,
  validateAnnouncementTitle,
  validateRetractReason,
  validateVersionFields,
  type Announcement,
  type AnnouncementKind,
  type AnnouncementListItem,
  type AnnouncementRetraction
} from './model';
import { AnnouncementService, type AnnouncementConfig } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  description: string;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    nodeInfo?: {
      peerId?: string;
      addresses: string[];
    };
  }>;
};

/** 卡片回调后高亮时长（ms）：足够用户注意到定位目标，又不永久占用视觉焦点 */
const HIGHLIGHT_DURATION_MS = 2500;

export default defineComponent({
  name: 'AnnouncementView',
  props: {
    pluginContext: {
      type: Object as () => { orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<AnnouncementService | null>(null);
    const loading = ref(false);
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');
    const config = ref<AnnouncementConfig | null>(null);

    // 两层导航：list（公告列表）→ detail（公告详情）
    const view = ref<'list' | 'detail'>('list');
    const kindFilter = ref<AnnouncementKind | 'all'>('all');
    const selectedAnnouncementId = ref('');

    const announcements = ref<Announcement[]>([]);
    const retractions = ref<AnnouncementRetraction[]>([]);

    // 发布对话框
    const publishDialogVisible = ref(false);
    const publishing = ref(false);
    const publishDraft = ref({ kind: 'notice' as AnnouncementKind, title: '', body: '', version: '', releaseRef: '' });

    // 撤回对话框
    const retractDialogVisible = ref(false);
    const retracting = ref(false);
    const retractReason = ref('');

    // 发布权配置对话框
    const configDialogVisible = ref(false);
    const configSaving = ref(false);
    const configDraft = ref({ publisherText: '', enableRelease: true, enableNotice: true });

    // 验签
    const verifying = ref(false);
    const verifyResult = ref('');
    // 撤回验签（与公告验签相互独立的结果槽）
    const retractionVerifying = ref(false);
    const retractionVerifyResult = ref('');

    const highlightedId = ref('');

    let highlightTimer: ReturnType<typeof setTimeout> | null = null;
    let offCardAction: (() => void) | null = null;

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const isAdmin = computed(() => canManageAnnounceConfig(currentOrgRole.value));
    const canPublish = computed(() => canPublishAnnouncement(config.value, currentRootId.value));

    /** 合法撤回人集合（读侧鉴权）：发布权集合 ∪ 名册管理员（fail-closed） */
    const retractorRootIds = computed<ReadonlySet<string>>(() => {
      const ids = new Set<string>(config.value?.publisherRootIds ?? []);
      for (const member of activeOrg.value?.members ?? []) {
        if (member.role === 'admin') {
          ids.add(member.rootId);
        }
      }
      return ids;
    });

    const retractionMap = computed(() => deriveRetractionMap(retractions.value, retractorRootIds.value));

    const listItems = computed<AnnouncementListItem[]>(() =>
      buildAnnouncementList(announcements.value, retractionMap.value, kindFilter.value)
    );

    const activeAnnouncement = computed(
      () => announcements.value.find((item) => item.id === selectedAnnouncementId.value) ?? null
    );

    const activeRetraction = computed(() =>
      activeAnnouncement.value ? findApplicableRetraction(activeAnnouncement.value, retractionMap.value) ?? null : null
    );

    const canRetractActive = computed(() =>
      canRetractAnnouncement(config.value, currentRootId.value, currentOrgRole.value)
    );

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureSdk = async () => {
      if (!sdk.value) {
        // SDK 由插件入口在桥握手完成时注入 window.__sparkPluginSDK，
        // 视图挂载可能先于握手完成，挂起等待注入
        sdk.value = await ensurePluginSDK();
        service.value = new AnnouncementService(sdk.value);
      }
      return sdk.value;
    };

    const loadOrganizations = async () => {
      const plugin = await ensureSdk();
      const all = await plugin.runtime.listMineOrganizations();

      // 组织与插件无绑定：全部已加入组织皆可选
      orgOptions.value = all as OrganizationView[];

      const preferredOrgId = props.pluginContext?.orgId;
      if (preferredOrgId && orgOptions.value.some((org) => org.orgId === preferredOrgId)) {
        selectedOrgId.value = preferredOrgId;
        return;
      }

      if (!orgOptions.value.some((org) => org.orgId === selectedOrgId.value)) {
        selectedOrgId.value = orgOptions.value[0]?.orgId ?? '';
      }
    };

    const loadAnnouncements = async () => {
      await ensureSdk();
      if (!service.value) {
        throw new Error('Plugin service unavailable');
      }
      if (!selectedOrgId.value) {
        announcements.value = [];
        retractions.value = [];
        config.value = null;
        return;
      }

      const orgId = selectedOrgId.value;
      const [loadedConfig, announcementList, retractionList] = await Promise.all([
        service.value.loadConfig(orgId),
        service.value.loadAnnouncements(orgId),
        service.value.loadRetractions(orgId)
      ]);
      config.value = loadedConfig;
      announcements.value = announcementList;
      retractions.value = retractionList;

      // 成员侧「本地生成」卡片（服务号模型 §20.4.3；档二-4 MVP：插件加载时
      // 补发 + 节流）。卡片生成失败不影响加载，仅降级少一条本机卡片。
      try {
        await service.value.notifyNewAnnouncements(orgId, announcementList, retractionList, retractorRootIds.value);
      } catch (error) {
        console.warn('[spark-announcement] 成员侧本地生成卡片失败（已降级）：', error);
      }
    };

    const syncLatestFromPeers = async (): Promise<boolean> => {
      if (!selectedOrgId.value) {
        return false;
      }

      const plugin = await ensureSdk();
      try {
        await plugin.runtime.syncOrganizationData(selectedOrgId.value);
        return true;
      } catch (error) {
        setMessage(`成员数据同步失败：${error}`, 'warning');
        return false;
      }
    };

    const reloadAll = async () => {
      loading.value = true;
      try {
        const plugin = await ensureSdk();
        const identity = await plugin.runtime.currentRoot();
        currentRootId.value = identity.rootId;

        await loadOrganizations();
        // 同步后台化，避免不可达 peer 阻塞首屏：先渲染本地数据，peer 同步
        // 在后台进行；同步成功后再走 loadAnnouncements 刷新（含本地卡片链路）。
        await loadAnnouncements();
        void syncLatestFromPeers().then(async (synced) => {
          if (synced) {
            await loadAnnouncements().catch(() => undefined);
          }
        });
      } catch (error) {
        setMessage(`加载失败：${error}`, 'error');
      } finally {
        loading.value = false;
      }
    };

    const onOrgChange = () => {
      view.value = 'list';
      selectedAnnouncementId.value = '';
      void loadAnnouncements().catch((error) => setMessage(`加载失败：${error}`, 'error'));
    };

    const enterDetail = (announcementId: string) => {
      selectedAnnouncementId.value = announcementId;
      verifyResult.value = '';
      retractionVerifyResult.value = '';
      view.value = 'detail';
    };

    // ---------------- 发布 ----------------

    const openPublishDialog = () => {
      publishDraft.value = {
        kind: config.value?.enableRelease === false ? 'notice' : 'release',
        title: '',
        body: '',
        version: '',
        releaseRef: ''
      };
      publishDialogVisible.value = true;
    };

    const submitPublish = async () => {
      const titleCheck = validateAnnouncementTitle(publishDraft.value.title);
      if (!titleCheck.ok) {
        ElMessage.warning(titleCheck.reason || '标题不合法');
        return;
      }
      const bodyCheck = validateAnnouncementBody(publishDraft.value.body);
      if (!bodyCheck.ok) {
        ElMessage.warning(bodyCheck.reason || '正文不合法');
        return;
      }
      const versionCheck = validateVersionFields(publishDraft.value.version, publishDraft.value.releaseRef);
      if (!versionCheck.ok) {
        ElMessage.warning(versionCheck.reason || '版本字段不合法');
        return;
      }

      publishing.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        const announcement = await service.value.publishAnnouncement(
          selectedOrgId.value,
          currentRootId.value,
          {
            kind: publishDraft.value.kind,
            title: publishDraft.value.title,
            body: publishDraft.value.body,
            version: publishDraft.value.version || undefined,
            releaseRef: publishDraft.value.releaseRef || undefined
          },
          config.value
        );
        publishDialogVisible.value = false;

        // 发布者本机即时反馈：为刚发布的公告生成本机卡片（记入送达台账，
        // 成员侧补发路径不会重复生成）
        const notified = await service.value.notifyAnnouncement(announcement);
        await loadAnnouncements();
        setMessage(
          notified
            ? '公告发布成功（已触发 P2P 同步，本机应用会话已生成公告卡片）'
            : '公告发布成功（已触发 P2P 同步；应用消息被权限/限流降级，未生成本机卡片）',
          'success'
        );
      } catch (error) {
        setMessage(`发布失败：${error}`, 'error');
      } finally {
        publishing.value = false;
      }
    };

    // ---------------- 撤回 ----------------

    const openRetractDialog = () => {
      retractReason.value = '';
      retractDialogVisible.value = true;
    };

    const submitRetract = async () => {
      const reasonCheck = validateRetractReason(retractReason.value);
      if (!reasonCheck.ok) {
        ElMessage.warning(reasonCheck.reason || '撤回理由不合法');
        return;
      }
      retracting.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value || !activeAnnouncement.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.retractAnnouncement(
          selectedOrgId.value,
          currentRootId.value,
          { targetAnnouncementId: activeAnnouncement.value.id, reason: retractReason.value || undefined },
          config.value,
          currentOrgRole.value
        );
        retractDialogVisible.value = false;
        await loadAnnouncements();
        setMessage('撤回记录已追加留痕（原公告不删除，已送达卡片不回滚、标注「已撤回」）', 'success');
      } catch (error) {
        setMessage(`撤回失败：${error}`, 'error');
      } finally {
        retracting.value = false;
      }
    };

    // ---------------- 发布权配置 ----------------

    const openConfigDialog = () => {
      configDraft.value = {
        publisherText: (config.value?.publisherRootIds ?? []).join('\n'),
        enableRelease: config.value?.enableRelease !== false,
        enableNotice: config.value?.enableNotice !== false
      };
      configDialogVisible.value = true;
    };

    const submitConfig = async () => {
      const publisherRootIds = configDraft.value.publisherText
        .split('\n')
        .map((item) => item.trim())
        .filter(Boolean);
      if (publisherRootIds.length === 0) {
        ElMessage.warning('发布者集合不能为空（至少登记一个 RootID）');
        return;
      }
      configSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.saveConfig(
          selectedOrgId.value,
          currentRootId.value,
          {
            publisherRootIds,
            enableRelease: configDraft.value.enableRelease,
            enableNotice: configDraft.value.enableNotice
          },
          currentOrgRole.value
        );
        configDialogVisible.value = false;
        await loadAnnouncements();
        setMessage('发布权配置已保存', 'success');
      } catch (error) {
        setMessage(`发布权配置保存失败：${error}`, 'error');
      } finally {
        configSaving.value = false;
      }
    };

    // ---------------- 验签 ----------------

    const verifyActiveAnnouncement = async () => {
      if (!activeAnnouncement.value) {
        return;
      }
      await ensureSdk();
      if (!service.value) {
        return;
      }
      verifying.value = true;
      try {
        const valid = await service.value.verifyAnnouncementSignature(activeAnnouncement.value);
        verifyResult.value = valid ? '验签通过：确为发布者域身份签发' : '验签失败：签名与内容不符';
      } catch (error) {
        verifyResult.value = `验签出错：${error}`;
      } finally {
        verifying.value = false;
      }
    };

    /** 撤回验签（免权限）：校验撤回记录确为撤回人域身份签发 */
    const verifyActiveRetraction = async () => {
      if (!activeRetraction.value) {
        return;
      }
      await ensureSdk();
      if (!service.value) {
        return;
      }
      retractionVerifying.value = true;
      try {
        const valid = await service.value.verifyRetractionSignature(activeRetraction.value);
        retractionVerifyResult.value = valid ? '验签通过：确为撤回人域身份签发' : '验签失败：签名与撤回内容不符';
      } catch (error) {
        retractionVerifyResult.value = `验签出错：${error}`;
      } finally {
        retractionVerifying.value = false;
      }
    };

    /**
     * 卡片回调（messages.onCardAction）：公告卡片「查看全文」经壳层归属校验
     * 后路由到这里。card.data 捎带 orgId——卡片可能属于非当前选中组织的应用
     * 会话，定位前先切换组织，再定位公告（未同步则先重载）。
     */
    const handleCardAction = async (action: PluginCardActionPayload) => {
      if (action.actionId !== 'view-announcement') {
        return;
      }
      const data = action.data as { announcementId?: string; orgId?: string } | undefined;
      const announcementId = data?.announcementId;
      if (!announcementId) {
        return;
      }
      if (data?.orgId && data.orgId !== selectedOrgId.value) {
        if (!orgOptions.value.some((org) => org.orgId === data.orgId)) {
          setMessage('目标公告所属组织不在本机已加入的组织中。', 'warning');
          return;
        }
        selectedOrgId.value = data.orgId;
        await loadAnnouncements().catch(() => undefined);
      }
      let target = announcements.value.find((item) => item.id === announcementId);
      if (!target) {
        // 目标公告不在当前视图（可能尚未同步）：重载一次再定位
        await loadAnnouncements().catch(() => undefined);
        target = announcements.value.find((item) => item.id === announcementId);
      }
      if (!target) {
        setMessage('目标公告尚未同步到本机，请稍后重试。', 'warning');
        return;
      }
      enterDetail(target.id);
      await nextTick();
      document
        .getElementById(`ann-detail-${target.id}`)
        ?.scrollIntoView({ behavior: 'smooth', block: 'start' });
      highlightedId.value = target.id;
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
      highlightTimer = setTimeout(() => {
        highlightedId.value = '';
      }, HIGHLIGHT_DURATION_MS);
    };

    const bodyPreview = (body: string): string => {
      const normalized = body.replace(/\s+/g, ' ').trim();
      return normalized.length > 80 ? `${normalized.slice(0, 80)}…` : normalized;
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

    onMounted(() => {
      void (async () => {
        const plugin = await ensureSdk();
        // 注册卡片回调（返回注销函数；仅 app 主视图注册，卡片视图收不到）
        offCardAction = plugin.messages?.onCardAction((action) => {
          void handleCardAction(action);
        }) ?? null;
        await reloadAll();
      })();
    });

    onUnmounted(() => {
      offCardAction?.();
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
    });

    watch(
      () => props.pluginContext?.orgId,
      (orgId) => {
        if (!orgId || selectedOrgId.value === orgId) {
          return;
        }
        if (!orgOptions.value.some((org) => org.orgId === orgId)) {
          return;
        }
        selectedOrgId.value = orgId;
        onOrgChange();
      }
    );

    return {
      loading,
      message,
      messageType,
      currentRootId,
      orgOptions,
      selectedOrgId,
      activeOrg,
      isAdmin,
      canPublish,
      config,
      view,
      kindFilter,
      listItems,
      activeAnnouncement,
      activeRetraction,
      canRetractActive,
      verifying,
      verifyResult,
      retractionVerifying,
      retractionVerifyResult,
      highlightedId,
      publishDialogVisible,
      publishing,
      publishDraft,
      retractDialogVisible,
      retracting,
      retractReason,
      configDialogVisible,
      configSaving,
      configDraft,
      reloadAll,
      onOrgChange,
      enterDetail,
      openPublishDialog,
      submitPublish,
      openRetractDialog,
      submitRetract,
      openConfigDialog,
      submitConfig,
      verifyActiveAnnouncement,
      verifyActiveRetraction,
      bodyPreview,
      formatDate
    };
  }
});
</script>

<style scoped>
.spark-announcement {
  display: grid;
  gap: 14px;
}

.header-card {
  border-radius: 12px;
}

.message {
  margin-bottom: 2px;
}

.header-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.header-row-inner {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.eyebrow {
  margin: 0 0 6px;
  color: #b45309;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

h2,
h3 {
  margin: 0;
}

.lede {
  margin: 8px 0 0;
  color: #64748b;
}

.selectors {
  margin-top: 12px;
}

.meta-row {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.root-id-tag {
  max-width: 100%;
}

.root-id-text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ann-item {
  border: 1px solid var(--el-border-color);
  border-radius: 10px;
  padding: 12px;
  margin-bottom: 10px;
  cursor: pointer;
  transition: border-color 0.3s, box-shadow 0.3s;
}

.ann-item:hover {
  border-color: #b45309;
}

.ann-item.retracted {
  opacity: 0.6;
}

.ann-item.highlighted {
  border-color: #b45309;
  box-shadow: 0 0 0 3px rgba(180, 83, 9, 0.18);
}

/* 卡片回调定位后公告详情短暂高亮（见 HIGHLIGHT_DURATION_MS） */
.detail-highlighted {
  border-color: #b45309;
  box-shadow: 0 0 0 3px rgba(180, 83, 9, 0.18);
  transition: box-shadow 0.3s;
}

.ann-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.ann-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ann-preview {
  margin: 6px 0;
  color: #64748b;
  font-size: 13px;
}

.ann-detail-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ann-state-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.retraction-alert {
  margin-bottom: 10px;
}

.post-meta {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
}

.post-meta .author {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.release-ref {
  color: #475569;
  font-size: 13px;
  margin: 8px 0 0;
}

.hint-inline {
  color: #94a3b8;
  font-size: 12px;
}

.ann-content {
  margin: 10px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.signature-info {
  color: #64748b;
  font-size: 12px;
  word-break: break-all;
}

.signature-info p {
  margin: 4px 0;
}

.ann-ops {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

.verify-result {
  color: #64748b;
  font-size: 12px;
}

.retraction-verify-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
}

.retract-target {
  margin: 0 0 10px;
  font-size: 13px;
  color: #334155;
  word-break: break-all;
}

.hint {
  color: #64748b;
  margin: 8px 0 0;
  font-size: 12px;
}

.retract-form {
  margin-top: 10px;
}
</style>
