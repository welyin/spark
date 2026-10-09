<!--
  问题反馈应用（spark-feedback）· 主视图（feedback.md v0.2 §4 + §7 MVP）。

  分区：
  - 提交表单：类型（缺陷/建议）、标题、描述、复现步骤（缺陷）、环境信息预览
    （opt-in 开关，ctx 缺省时手填版本号降级）、附件区（opt-in 逐件勾选后上传，
    默认不勾；日志类附件逐件隐私提示）；
  - 目标议题：档三-15 三路兜底——偏好/内置常量自动解析 → 关注列表选择 →
    创世记录粘贴关注；元数据不可达时允许先存草稿后补投（验收第 3 条）；
  - 公开性明示：目标议题公开时顶部固定横幅 + 独立确认勾选，未确认不可提交
    （确认动作与提交按钮分离，验收第 1 条）；
  - 手动兜底：导出预填创世草稿 JSON（档一-1 兜底路径），引导到「项目」插件
    确认提交；导出内容同样受不可撤回告知约束；
  - 我的反馈：台账 + 本地副本所见的决议状态如实呈现（未同步到显示
    「本地副本未同步」，不编造状态）；
  - 每日提交数温馨提示（档三-18）：达阈值仅提示不阻断。

  诚实口径：操作者身份 = 本插件域身份 id（平台暂无个人身份签名面，同
  spark-affairs）；能力缺失一律如实降级（affairs 缺 → 草稿/导出可用）。
-->
<template>
  <section class="spark-feedback">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">问题反馈</p>
          <h2>对运行版本提交缺陷与建议</h2>
          <p class="lede">反馈一键回流为目标项目议题的子事务（bug / proposal），签名存证、可讨论可决议。</p>
        </div>
        <el-button size="small" @click="reloadAll" :loading="loading">刷新</el-button>
      </div>
    </el-card>

    <el-alert v-if="!caps.affairs" type="warning" :closable="false" show-icon class="message"
      title="当前宿主未提供事务模块（sdk.affairs）：直建子事务回流不可用。仍可填写并保存草稿，或手动导出到「项目」插件提交。" />

    <el-alert v-if="dailyNotice" type="warning" :closable="false" show-icon class="message" :title="dailyNotice" />

    <!-- ============ 目标议题 ============ -->
    <el-card shadow="never" class="block">
      <template #header><strong>目标议题</strong></template>
      <div class="target-row">
        <el-select v-model="form.targetAffairId" size="small" style="width: 320px" placeholder="从已关注的议题中选择"
          :disabled="!caps.affairs" @change="onTargetChange">
          <el-option v-for="topic in topics" :key="topic.affairId"
            :label="`${topic.title}（${topic.affairId.slice(0, 12)}…）`" :value="topic.affairId" />
        </el-select>
        <el-checkbox v-model="setDefaultTarget" size="small">设为默认目标</el-checkbox>
      </div>
      <template v-if="targetMeta">
        <p class="meta-line">目标：<strong>{{ targetMeta.title }}</strong>
          <el-tag v-if="targetMeta.isPublic" type="danger" size="small">公开议题</el-tag>
          <el-tag v-else type="info" size="small">非公开</el-tag>
        </p>
      </template>
      <p v-else-if="form.targetAffairId" class="meta-line warn">
        目标议题元数据不可达（未关注或未同步）——可先存草稿，可达后补投。
      </p>
      <el-collapse class="follow-box">
        <el-collapse-item title="通过邀请 / 创世记录关注新议题" name="follow">
          <el-input v-model="followJson" type="textarea" :rows="3" placeholder="粘贴议题创世记录 JSON（邀请转发件）" />
          <el-button size="small" class="follow-btn" :disabled="!caps.affairs" @click="doFollow">关注</el-button>
        </el-collapse-item>
      </el-collapse>
    </el-card>

    <!-- ============ 公开性明示（验收第 1 条：横幅 + 独立确认） ============ -->
    <el-alert v-if="targetMeta?.isPublic" type="error" :closable="false" show-icon class="message"
      :title="`此反馈将公开发布到「${targetMeta.title}」议题，内容（含附件）将随事务复制面公开扩散、不可撤回。`" />

    <!-- ============ 反馈表单 ============ -->
    <el-card shadow="never" class="block">
      <template #header><strong>反馈内容</strong></template>

      <div class="form-row">
        <span class="label">类型</span>
        <el-radio-group v-model="form.type" size="small">
          <el-radio-button value="bug">缺陷</el-radio-button>
          <el-radio-button value="proposal">建议</el-radio-button>
        </el-radio-group>
      </div>

      <el-input v-model="form.title" placeholder="标题（2–80 字符）" maxlength="80" show-word-limit class="field" />
      <el-input v-model="form.body" type="textarea" :rows="5" placeholder="描述（10–4000 字符）" maxlength="4000"
        show-word-limit class="field" />
      <el-input v-if="form.type === 'bug'" v-model="form.reproduction" type="textarea" :rows="3"
        placeholder="复现步骤（可选）" maxlength="4000" show-word-limit class="field" />

      <!-- 环境信息（opt-in；ctx 缺省时手填版本号降级） -->
      <div class="form-row">
        <el-checkbox v-model="form.includeEnvironment" size="small">附带环境信息（可见可关）</el-checkbox>
      </div>
      <div v-if="form.includeEnvironment" class="env-preview">
        <code>{{ envPreviewText }}</code>
        <el-input v-if="!caps.environment" v-model="form.reportedVersion" size="small"
          placeholder="当前宿主未注入版本信息，请手填运行版本号（如 0.3.2）" class="field" />
      </div>

      <!-- 附件（opt-in 逐件勾选，默认不勾） -->
      <template v-if="caps.content">
        <div class="form-row">
          <span class="label">附件</span>
          <input ref="fileInput" type="file" multiple accept="image/*,.log,.txt,.json" class="file-input"
            @change="onFilesPicked" />
          <el-button size="small" @click="pickFiles">选择文件</el-button>
          <span class="hint">默认不附带；勾选即上传（cid 入反馈载荷）。截图/日志可能包含路径、联系人等个人信息，请自查。</span>
        </div>
        <div v-for="candidate in candidates" :key="candidate.key" class="attachment-line">
          <el-checkbox :model-value="candidate.uploaded" size="small" :disabled="candidate.uploading"
            @change="(checked: boolean) => toggleAttachment(candidate, checked)">
            {{ candidate.name }}（{{ formatSize(candidate.size) }}）
          </el-checkbox>
          <el-tag v-if="candidate.size > sizeHint" type="warning" size="small">体积较大</el-tag>
          <el-button text size="small" type="danger" @click="removeCandidate(candidate)">移除</el-button>
        </div>
      </template>
      <p v-else class="hint">当前宿主未提供内容面模块，附件功能不可用。</p>

      <!-- 提交确认（公开议题逐次确认，动作与提交按钮分离） -->
      <div v-if="targetMeta?.isPublic" class="form-row">
        <el-checkbox v-model="confirmedPublic" size="small">
          我已知晓：本反馈内容（含附件）将公开扩散且不可撤回
        </el-checkbox>
      </div>

      <div class="actions">
        <el-button type="primary" :loading="busy" :disabled="!canSubmit" @click="doSubmit">提交反馈</el-button>
        <el-button :disabled="busy" @click="doSaveDraft">存草稿</el-button>
        <el-button :disabled="busy" @click="doExport">手动导出</el-button>
      </div>
      <p v-if="targetMeta?.isPublic && !confirmedPublic" class="hint">提交前请确认上方公开告知。</p>
    </el-card>

    <!-- ============ 草稿 ============ -->
    <el-card v-if="drafts.length > 0" shadow="never" class="block">
      <template #header><strong>草稿（本机）</strong></template>
      <div v-for="draft in drafts" :key="draft.id" class="ledger-line">
        <el-tag size="small">{{ draft.type === 'bug' ? '缺陷' : '建议' }}</el-tag>
        <span class="ledger-title">{{ draft.title || '（无标题）' }}</span>
        <span class="muted">{{ formatTime(draft.savedAt) }}</span>
        <el-button text size="small" @click="applyDraft(draft)">继续填写</el-button>
        <el-button text size="small" type="danger" @click="doDeleteDraft(draft)">删除</el-button>
      </div>
    </el-card>

    <!-- ============ 我的反馈 ============ -->
    <el-card shadow="never" class="block">
      <template #header><strong>我的反馈</strong></template>
      <el-empty v-if="ledger.length === 0" description="还没有提交过反馈" />
      <div v-for="entry in ledger" :key="entry.id" class="ledger-line">
        <el-tag size="small" :type="entry.type === 'bug' ? 'warning' : 'primary'">
          {{ entry.type === 'bug' ? '缺陷' : '建议' }}
        </el-tag>
        <span class="ledger-title">{{ entry.title }}</span>
        <el-tag size="small" :type="statusTagType(entry.status)">{{ statusLabel(entry.status) }}</el-tag>
        <span class="muted">{{ formatTime(entry.submittedAt) }}</span>
        <el-button text size="small" @click="openAffair(entry.childAffairId)">查看子事务</el-button>
        <el-button text size="small" type="danger" @click="doRemoveEntry(entry)">删除记录</el-button>
      </div>
      <p class="hint">状态为本地副本所见：未同步到决议时如实显示「暂无后续」。删除记录只移除本插件台账指针，已回流的子事务仍归目标议题所有。</p>
    </el-card>

    <!-- ============ 导出对话框 ============ -->
    <el-dialog v-model="exportVisible" title="手动导出（兜底路径）" width="640px">
      <el-alert type="warning" :closable="false" show-icon class="message"
        title="以下内容一经在「项目」插件确认提交，公开性归目标议题规则管：公开议题上不可撤回。" />
      <el-input v-model="exportText" type="textarea" :rows="14" readonly />
      <template #footer>
        <el-button @click="copyExport">复制</el-button>
        <el-button type="primary" @click="exportVisible = false">完成</el-button>
      </template>
    </el-dialog>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import type { PluginContext, PluginSDK } from '../../packages/plugin-sdk/src';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import {
  ATTACHMENT_SIZE_HINT,
  dailyLimitNotice,
  isValidAffairId,
  STATUS_LABELS,
  type FeedbackAttachment,
  type FeedbackDraft,
  type FeedbackType,
  type LedgerView
} from './model';
import { FeedbackService, probeCapabilities, type PluginCapabilities } from './service';

const props = defineProps<{ ctx?: Partial<PluginContext> }>();

type Candidate = {
  key: string;
  name: string;
  size: number;
  mime?: string;
  dataBase64: string;
  uploaded: boolean;
  uploading: boolean;
  attachment?: FeedbackAttachment;
};

const sdk = ref<PluginSDK | null>(null);
const service = ref<FeedbackService | null>(null);
const caps = ref<PluginCapabilities>({ affairs: false, content: false, messages: false, environment: false });

const loading = ref(false);
const busy = ref(false);
const message = ref('');
const messageType = ref<'success' | 'warning' | 'error' | 'info'>('info');

/** 当前表单草稿 id（附件 pinRoot 与草稿存档共用键） */
const draftId = `draft-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

const form = reactive({
  type: 'bug' as FeedbackType,
  title: '',
  body: '',
  reproduction: '',
  includeEnvironment: true,
  reportedVersion: '',
  targetAffairId: '',
  attachments: [] as FeedbackAttachment[]
});
const candidates = ref<Candidate[]>([]);
const confirmedPublic = ref(false);
const setDefaultTarget = ref(false);

const topics = ref<Array<{ affairId: string; title: string; type: string }>>([]);
const targetMeta = ref<{ title: string; isPublic: boolean } | null>(null);
const followJson = ref('');
const ledger = ref<LedgerView[]>([]);
const drafts = ref<FeedbackDraft[]>([]);
const todayCount = ref(0);

const exportVisible = ref(false);
const exportText = ref('');
const fileInput = ref<HTMLInputElement | null>(null);

const sizeHint = ATTACHMENT_SIZE_HINT;

const envPreviewText = computed(() => {
  if (!service.value) {
    return '';
  }
  const env = service.value.buildEnvironmentPreview(form.reportedVersion || undefined);
  return JSON.stringify(env);
});

const dailyNotice = computed(() => dailyLimitNotice(todayCount.value));

const canSubmit = computed(() => {
  if (!caps.value.affairs || busy.value) {
    return false;
  }
  if (!isValidAffairId(form.targetAffairId) || !targetMeta.value) {
    return false;
  }
  if (targetMeta.value.isPublic && !confirmedPublic.value) {
    return false;
  }
  return form.title.trim().length >= 2 && form.body.trim().length >= 10;
});

function showMessage(text: string, type: 'success' | 'warning' | 'error' | 'info' = 'info'): void {
  message.value = text;
  messageType.value = type;
}

function formatTime(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : '';
}

function formatSize(size: number): string {
  if (size >= 1024 * 1024) {
    return `${(size / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${Math.max(1, Math.round(size / 1024))} KB`;
}

function statusLabel(status: LedgerView['status']): string {
  return STATUS_LABELS[status];
}

function statusTagType(status: LedgerView['status']): 'success' | 'warning' | 'info' | 'danger' {
  if (status === 'effective') {
    return 'success';
  }
  if (status === 'pending') {
    return 'warning';
  }
  if (status === 'vetoed') {
    return 'danger';
  }
  return 'info';
}

async function onTargetChange(): Promise<void> {
  confirmedPublic.value = false;
  targetMeta.value = form.targetAffairId ? await service.value?.targetMeta(form.targetAffairId) ?? null : null;
  if (setDefaultTarget.value && isValidAffairId(form.targetAffairId)) {
    const prefs = await service.value!.getPrefs();
    await service.value!.savePrefs({ ...prefs, defaultTargetAffairId: form.targetAffairId });
  }
}

async function doFollow(): Promise<void> {
  if (!service.value) {
    return;
  }
  try {
    const genesis = JSON.parse(followJson.value);
    const affairId = await service.value.followGenesis(genesis);
    showMessage(`已关注议题 ${affairId.slice(0, 12)}…（关注即副本语义）`, 'success');
    followJson.value = '';
    form.targetAffairId = affairId;
    await reloadTargets();
    await onTargetChange();
  } catch (error) {
    showMessage(`关注失败：${(error as Error).message}`, 'error');
  }
}

function pickFiles(): void {
  fileInput.value?.click();
}

async function onFilesPicked(event: Event): Promise<void> {
  const files = (event.target as HTMLInputElement).files;
  if (!files) {
    return;
  }
  for (const file of Array.from(files)) {
    const dataBase64 = await readFileBase64(file);
    candidates.value.push({
      key: `${file.name}-${file.size}-${file.lastModified}`,
      name: file.name,
      size: file.size,
      ...(file.type ? { mime: file.type } : {}),
      dataBase64,
      uploaded: false,
      uploading: false
    });
  }
  (event.target as HTMLInputElement).value = '';
}

function readFileBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(',')[1] ?? '');
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

/** 勾选 = 上传并入载荷（opt-in 逐件确认）；取消勾选 = 解除根标记并移出载荷 */
async function toggleAttachment(candidate: Candidate, checked: boolean): Promise<void> {
  if (!service.value) {
    return;
  }
  if (checked) {
    candidate.uploading = true;
    try {
      const attachment = await service.value.uploadAttachment(draftId, {
        dataBase64: candidate.dataBase64,
        name: candidate.name,
        ...(candidate.mime ? { mime: candidate.mime } : {})
      });
      candidate.attachment = attachment;
      candidate.uploaded = true;
      form.attachments.push(attachment);
    } catch (error) {
      showMessage(`附件上传失败：${(error as Error).message}`, 'error');
    } finally {
      candidate.uploading = false;
    }
  } else if (candidate.attachment) {
    await service.value.discardAttachment(draftId, candidate.attachment.cid);
    form.attachments = form.attachments.filter((a) => a.cid !== candidate.attachment!.cid);
    candidate.uploaded = false;
    candidate.attachment = undefined;
  }
}

async function removeCandidate(candidate: Candidate): Promise<void> {
  if (candidate.attachment) {
    await service.value?.discardAttachment(draftId, candidate.attachment.cid);
    form.attachments = form.attachments.filter((a) => a.cid !== candidate.attachment!.cid);
  }
  candidates.value = candidates.value.filter((c) => c.key !== candidate.key);
}

function currentInput() {
  return {
    type: form.type,
    title: form.title,
    body: form.body,
    reproduction: form.reproduction,
    includeEnvironment: form.includeEnvironment,
    attachments: [...form.attachments],
    reportedVersion: form.reportedVersion
  };
}

async function doSubmit(): Promise<void> {
  if (!service.value) {
    return;
  }
  busy.value = true;
  try {
    const { entry, cardSent } = await service.value.submit(currentInput(), form.targetAffairId, {
      draftId,
      confirmedPublic: confirmedPublic.value
    });
    showMessage(
      `反馈已提交为子事务 ${entry.childAffairId.slice(0, 12)}…${cardSent ? '，回执卡片已发送到应用会话' : ''}`,
      'success'
    );
    resetForm();
    await reloadLedger();
  } catch (error) {
    showMessage(`提交失败：${(error as Error).message}——可重试，或先存草稿/手动导出`, 'error');
  } finally {
    busy.value = false;
  }
}

async function doSaveDraft(): Promise<void> {
  if (!service.value) {
    return;
  }
  try {
    await service.value.saveDraft({
      id: draftId,
      type: form.type,
      title: form.title,
      body: form.body,
      reproduction: form.reproduction,
      attachments: [...form.attachments],
      targetAffairId: form.targetAffairId
    });
    showMessage('草稿已保存（仅本机，不进入同步流量）', 'success');
    drafts.value = await service.value.listDrafts();
  } catch (error) {
    showMessage(`草稿保存失败：${(error as Error).message}`, 'error');
  }
}

function doExport(): void {
  if (!service.value) {
    return;
  }
  if (!isValidAffairId(form.targetAffairId)) {
    showMessage('请先选择或粘贴目标议题 affairId 再导出', 'warning');
    return;
  }
  try {
    exportText.value = service.value.buildExport(currentInput(), form.targetAffairId);
    exportVisible.value = true;
  } catch (error) {
    showMessage(`导出失败：${(error as Error).message}`, 'error');
  }
}

async function copyExport(): Promise<void> {
  try {
    await navigator.clipboard.writeText(exportText.value);
    showMessage('已复制到剪贴板', 'success');
  } catch {
    showMessage('剪贴板不可用——请手动全选复制文本框内容', 'warning');
  }
}

function applyDraft(draft: FeedbackDraft): void {
  form.type = draft.type;
  form.title = draft.title;
  form.body = draft.body;
  form.reproduction = draft.reproduction;
  form.attachments = [...draft.attachments];
  form.targetAffairId = draft.targetAffairId;
  candidates.value = draft.attachments.map((a) => ({
    key: `restored-${a.cid.slice(0, 12)}`,
    name: a.name,
    size: a.size,
    ...(a.mime ? { mime: a.mime } : {}),
    dataBase64: '',
    uploaded: true,
    uploading: false,
    attachment: a
  }));
  // 恢复草稿的附件在原草稿 id 下持有根标记；为当前表单会话补挂同 cid 的
  // 根标记（根标记按 cid × root 计，提交/删除时各自解除，互不误伤）
  if (service.value && caps.value.content) {
    void service.value.retainDraftAttachments(draftId, draft.attachments);
  }
  void onTargetChange();
}

async function doDeleteDraft(draft: FeedbackDraft): Promise<void> {
  await service.value?.deleteDraft(draft);
  drafts.value = (await service.value?.listDrafts()) ?? [];
}

async function doRemoveEntry(entry: LedgerView): Promise<void> {
  if (!service.value) {
    return;
  }
  try {
    await service.value.removeLedgerEntry(entry);
    showMessage('台账记录已删除（已回流的子事务仍归目标议题所有，不受影响）', 'success');
    await reloadLedger();
  } catch (error) {
    showMessage(`删除失败：${(error as Error).message}`, 'error');
  }
}

function openAffair(affairId: string): void {
  sdk.value?.navigation
    ?.openPlugin({ pluginId: 'spark-affairs', cardData: { affairId } })
    .catch((error) => showMessage(`打开事务界面失败：${(error as Error).message}`, 'error'));
}

function resetForm(): void {
  form.title = '';
  form.body = '';
  form.reproduction = '';
  form.attachments = [];
  candidates.value = [];
  confirmedPublic.value = false;
}

async function reloadTargets(): Promise<void> {
  if (!service.value || !caps.value.affairs) {
    return;
  }
  topics.value = await service.value.listFollowedTopics();
  if (!form.targetAffairId) {
    const resolved = await service.value.resolveTarget();
    if (resolved) {
      form.targetAffairId = resolved.affairId;
    }
  }
  targetMeta.value = form.targetAffairId ? await service.value.targetMeta(form.targetAffairId) : null;
}

async function reloadLedger(): Promise<void> {
  if (!service.value) {
    return;
  }
  ledger.value = await service.value.listLedgerView();
  todayCount.value = await service.value.countToday();
}

async function reloadAll(): Promise<void> {
  loading.value = true;
  try {
    await reloadTargets();
    await reloadLedger();
    drafts.value = (await service.value?.listDrafts()) ?? [];
  } catch (error) {
    showMessage(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
  }
}

onMounted(async () => {
  try {
    const instance = await ensurePluginSDK();
    sdk.value = instance;
    const svc = new FeedbackService(instance, props.ctx ?? null);
    service.value = svc;
    caps.value = probeCapabilities(instance, props.ctx);
    const prefs = await svc.getPrefs();
    form.includeEnvironment = prefs.includeEnvironment ?? true;
    // 卡片按钮回调（「查看子事务」→ 深链打开事务界面）
    instance.messages?.onCardAction((action) => {
      const affairId = (action.data as { affairId?: string } | undefined)?.affairId;
      if (action.actionId === 'open-affair' && affairId) {
        openAffair(affairId);
      }
    });
    await reloadAll();
  } catch (error) {
    showMessage(`初始化失败：${(error as Error).message}`, 'error');
  }
});
</script>

<style scoped>
.spark-feedback {
  padding: 16px;
  font-size: 13px;
  line-height: 1.6;
}
.message {
  margin-bottom: 12px;
}
.header-card,
.block {
  margin-bottom: 12px;
}
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
}
.eyebrow {
  margin: 0;
  color: #999;
  font-size: 12px;
}
h2 {
  margin: 2px 0;
  font-size: 18px;
}
.lede {
  margin: 0;
  color: #666;
}
.target-row,
.form-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
  flex-wrap: wrap;
}
.label {
  color: #666;
  flex: none;
}
.field {
  margin-bottom: 10px;
}
.env-preview {
  margin: 0 0 10px 24px;
}
.env-preview code {
  display: block;
  background: #f5f5f5;
  border-radius: 4px;
  padding: 6px 8px;
  margin-bottom: 6px;
  word-break: break-all;
}
.file-input {
  display: none;
}
.attachment-line {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-left: 24px;
}
.hint {
  margin: 4px 0 0;
  color: #999;
  font-size: 12px;
}
.meta-line {
  margin: 4px 0;
}
.meta-line.warn {
  color: #d46b08;
}
.follow-box {
  margin-top: 8px;
}
.follow-btn {
  margin-top: 6px;
}
.actions {
  display: flex;
  gap: 10px;
  margin-top: 4px;
}
.ledger-line {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
  border-bottom: 1px solid #f0f0f0;
  flex-wrap: wrap;
}
.ledger-title {
  font-weight: 500;
}
.muted {
  color: #999;
  font-size: 12px;
}
</style>
