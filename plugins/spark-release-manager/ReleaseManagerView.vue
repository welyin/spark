<!--
  发布管理插件（spark-release-manager）· 主视图（app 视图）。

  设计依据：release-management.md v0.2 §4（界面与交互要点）+ 拍板口径：
  - 三个区：版本列表（状态五态 / 渠道进度 / 包哈希截断可复制 / 发布者与时间；
    核验失败红色标注原因原文）｜发布详情（资产逐项 + 存证状态 + 状态事件时间线
    + 关联事务引用）｜渠道与更新跟踪（渠道最近一次推送 + 版本分布，如实标注
    「仅含 opt-in 上报成员，非全量」）；
  - 登记向导：粘贴 update-manifest.json 解析资产清单 → 人工核对 → 补变更说明
    与关联事务 → 签名提交；向导不替发布者做判断，核验未过不得推进「已发布」；
  - 诚实呈现：撤回不删除痕迹；存证链头哈希原样展示不伪造时效；信任级原样；
  - 降级（档二-8）：market 模块缺席 / 移动端 → 只做登记，核验入口隐藏并提示
    「委托桌面端成员核验」。
-->
<template>
  <section class="spark-release-manager">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">发布管理</p>
          <h2>构建产物登记 · 核验编排 · 渠道跟踪</h2>
          <p class="lede">发布单与包哈希入存证链（governance append-only）；构建在 CI，本插件只登记/编排/跟踪，只验不签。</p>
        </div>
        <el-button @click="reloadAll" :loading="loading">刷新</el-button>
      </div>

      <el-form v-if="orgOptions.length > 0" label-position="top" class="selectors">
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

      <el-empty v-if="orgOptions.length === 0 && !loading" description="你还没有加入任何组织。" />

      <div v-if="spaceReady" class="meta-row">
        <el-tag :type="isAdmin ? 'danger' : 'warning'">{{ isAdmin ? '组织管理员' : '组织成员' }}</el-tag>
        <el-tag :type="canPublish ? 'success' : 'info'">{{ canPublish ? '发布权集合成员' : '非发布权集合成员' }}</el-tag>
        <el-tag :type="marketAvailable ? 'success' : 'info'">
          {{ marketAvailable ? '市场模块已连接（可本机导入复算）' : '市场模块不可用（降级只做登记）' }}
        </el-tag>
        <el-tag v-if="isMobile" type="warning">移动端：降级只做登记，核验请委托桌面端成员（档二-8）</el-tag>
      </div>

      <div v-if="spaceReady" class="board-row">
        <el-button v-if="canPublish" type="primary" size="small" @click="openRegisterDialog">登记发布单</el-button>
        <el-button v-if="isAdmin" size="small" @click="configDialogVisible = true">发布权配置</el-button>
        <el-button v-if="canPublish || isAdmin" size="small" @click="openChannelDialog()">登记渠道</el-button>
      </div>
    </el-card>

    <el-alert
      v-if="spaceReady && configLoaded && !config"
      type="warning"
      :closable="false"
      show-icon
      class="message"
      title="发布权配置尚未初始化——任何成员都无登记路径（fail-closed）。请组织管理员在「发布权配置」中初始化发布权集合（档三-23）。"
    />

    <el-tabs v-if="spaceReady" v-model="activeTab" class="main-tabs">
      <!-- 版本列表 -->
      <el-tab-pane label="版本列表" name="releases">
        <el-empty v-if="releaseRows.length === 0" description="暂无发布单，请发布权集合成员登记" />
        <div v-for="row in releaseRows" :key="row.release.id" class="release-row" @click="openDetail(row.release.id)">
          <div class="release-main">
            <div class="release-title-row">
              <strong>{{ pluginDisplayName(row.release.pluginId) }}</strong>
              <span class="version">v{{ row.release.version }}</span>
              <el-tag size="small" :type="stateTagType(row.state)">{{ RELEASE_STATE_LABELS[row.state] }}</el-tag>
              <el-tag v-if="row.release.signature" size="small" type="success">已签名</el-tag>
            </div>
            <p v-if="row.state === 'verify-failed' && row.failureReason" class="failure">核验失败：{{ row.failureReason }}</p>
            <div class="release-meta">
              <span class="hash" :title="row.packageSha256" @click.stop="copyHash(row.packageSha256)">
                包哈希 {{ shortHash(row.packageSha256) }}（点击复制）
              </span>
              <span>渠道 {{ row.pushedChannelCount }}/{{ row.release.channels.length || 0 }}</span>
              <span>发布者 {{ row.release.publisherRootId.slice(0, 12) }}…</span>
              <span>{{ formatDate(row.release.createdAt) }}</span>
            </div>
          </div>
        </div>
      </el-tab-pane>

      <!-- 渠道与更新跟踪 -->
      <el-tab-pane label="渠道与更新跟踪" name="channels">
        <el-card shadow="never" class="panel-card">
          <template #header>渠道清单（声明性登记，不携带执行凭据；分发执行是内核/协议的事）</template>
          <el-empty v-if="channels.length === 0" description="暂无渠道，请先登记" />
          <div v-for="channel in channels" :key="channel.id" class="channel-row">
            <el-tag size="small">{{ RELEASE_CHANNEL_KIND_LABELS[channel.kind] }}</el-tag>
            <span class="channel-target">{{ channel.target }}</span>
            <span class="channel-latest">
              <template v-if="latestPushOf(channel.id)">
                最近推送 {{ formatDate(latestPushOf(channel.id)!.at) }}（操作者 {{ latestPushOf(channel.id)!.operatorRootId.slice(0, 12) }}…）
              </template>
              <template v-else>尚未推送</template>
            </span>
          </div>
        </el-card>

        <el-card shadow="never" class="panel-card">
          <template #header>
            版本分布
            <span class="honest-note">仅含 opt-in 上报成员，非全量（档三-25）</span>
          </template>
          <div class="report-row">
            <el-select v-model="distributionPluginId" placeholder="选择目标插件" size="small" class="report-plugin">
              <el-option v-for="pid in knownPluginIds" :key="pid" :label="pluginDisplayName(pid)" :value="pid" />
            </el-select>
            <template v-if="distribution">
              <el-tag size="small" type="info">上报 {{ distribution.reporterCount }} 人</el-tag>
              <el-tag size="small" :type="distribution.behindCount > 0 ? 'warning' : 'success'">
                滞后 {{ distribution.behindCount }} 人
              </el-tag>
            </template>
          </div>
          <template v-if="distribution && distribution.reporterCount > 0">
            <div v-for="row in distribution.versions" :key="row.version" class="dist-row">
              <span class="dist-version">v{{ row.version }}</span>
              <el-progress :percentage="distPercentage(row.count)" :stroke-width="10" />
              <span class="dist-count">{{ row.count }} 人</span>
            </div>
            <div class="dist-trusts">
              信任级分布：
              <el-tag v-for="row in distribution.trusts" :key="row.trust" size="small" class="dist-trust">
                {{ row.trust }} × {{ row.count }}
              </el-tag>
            </div>
          </template>
          <el-empty v-else description="该插件暂无版本上报" />

          <el-divider content-position="left">上报本机运行版本（opt-in，最小字段，不含设备指纹）</el-divider>
          <div class="report-row">
            <el-input v-model="reportDraft.version" size="small" placeholder="本机运行版本（如 0.1.0）" class="report-version" />
            <el-select v-model="reportDraft.trust" size="small" placeholder="信任级" class="report-trust">
              <el-option label="signed（签名链）" value="signed" />
              <el-option label="repo-anchored（仓库锚定）" value="repo-anchored" />
              <el-option label="sideloaded（侧载）" value="sideloaded" />
              <el-option label="builtin（内置）" value="builtin" />
            </el-select>
            <el-button size="small" :loading="reportSaving" :disabled="!distributionPluginId" @click="submitReport">上报</el-button>
          </div>
        </el-card>

        <el-card v-if="marketAvailable" shadow="never" class="panel-card">
          <template #header>当前版本 → 最新版本对照（内置目录签名链路探测结果原样展示）</template>
          <el-button size="small" :loading="probeLoading" @click="runUpdateProbe">探测更新</el-button>
          <div v-for="probe in updateProbes" :key="probe.pluginId" class="probe-row">
            <span>{{ pluginDisplayName(probe.pluginId) }}</span>
            <el-tag size="small" :type="probe.updateAvailable ? 'warning' : 'success'">
              {{ probe.updateAvailable ? `可更新 → ${probe.latestVersion ?? '?'}` : '已是最新' }}
            </el-tag>
            <span class="probe-reason">{{ probe.reason }}</span>
          </div>
        </el-card>
      </el-tab-pane>
    </el-tabs>

    <!-- 登记向导（发布者）：导入 update-manifest.json → 人工核对 → 补变更说明/关联事务 → 签名提交 -->
    <el-dialog v-model="registerDialogVisible" title="登记发布单" width="640px">
      <el-form label-position="top">
        <el-form-item label="① 粘贴 CI 产出的 update-manifest.json（自动解析资产清单）">
          <el-input v-model="registerDraft.updateManifestJson" type="textarea" :rows="5" placeholder='{"pluginId":"…","version":"0.1.0","assets":[{"kind":"package","fileName":"….spkg","sha256":"…","size":123}]}' />
        </el-form-item>
        <el-button size="small" @click="parseManifestDraft">解析资产清单</el-button>

        <template v-if="registerDraft.parsedArtifacts.length > 0">
          <el-divider content-position="left">② 人工核对资产清单（登记值将钉进存证链供事后比对）</el-divider>
          <div v-for="(asset, index) in registerDraft.parsedArtifacts" :key="index" class="artifact-row">
            <el-tag size="small">{{ asset.kind }}</el-tag>
            <span class="artifact-name">{{ asset.fileName }}</span>
            <span class="artifact-hash">{{ shortHash(asset.sha256) }}</span>
            <span>{{ asset.size }} B</span>
          </div>
        </template>

        <el-form-item label="目标插件（仓库地址形态，名字可抢注 URL 不可抢注）">
          <el-input v-model="registerDraft.pluginId" :maxlength="200" placeholder="如 https://github.com/org/repo" />
        </el-form-item>
        <el-form-item label="版本号（semver）">
          <el-input v-model="registerDraft.version" :maxlength="40" placeholder="如 0.1.0" />
        </el-form-item>
        <el-form-item label="变更说明（纯文本；长文走内容面 blob 引用排后续迭代）">
          <el-input v-model="registerDraft.changelog" type="textarea" :rows="4" :maxlength="20000" show-word-limit />
        </el-form-item>
        <el-form-item label="关联事务（可选，发布决议 / 里程碑议题 id，只存引用）">
          <el-input v-model="registerDraft.decisionRef" :maxlength="120" />
        </el-form-item>
        <el-form-item label="目标渠道（可选，先在「渠道」区登记）">
          <el-select v-model="registerDraft.channels" multiple placeholder="选择渠道">
            <el-option
              v-for="channel in channels"
              :key="channel.id"
              :label="`${RELEASE_CHANNEL_KIND_LABELS[channel.kind]} · ${channel.target}`"
              :value="channel.id"
            />
          </el-select>
        </el-form-item>
      </el-form>
      <p class="hint">
        登记即域身份签名（防抵赖）并写入 governance append-only 集合——发布单内容（含包哈希）随之进入存证链，不可编辑；
        纠错 = 追加状态事件或撤回。签名材料（sig/pubkey 资产）随 update-manifest 之外的 CI 产物由发布者核对后经后续迭代补登。
        登记幂等键 = （组织, 插件, 版本号），重复登记将被拒绝。
      </p>
      <template #footer>
        <el-button @click="registerDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="registerSaving" @click="submitRegister">签名并登记</el-button>
      </template>
    </el-dialog>

    <!-- 发布权配置（档三-23：名册管理员直改初始化） -->
    <el-dialog v-model="configDialogVisible" title="发布权配置" width="520px">
      <el-form label-position="top">
        <el-form-item label="发布权集合（域身份 rootId，每行一个）">
          <el-input v-model="configDraft.publisherRootIds" type="textarea" :rows="6" placeholder="64 位 rootId，每行一个" />
        </el-form-item>
      </el-form>
      <p class="hint">发布权（供应链敏感）的后续变更挂组织治理事务，排「规则挂事务」迭代（档三-23）；MVP 由名册管理员直改。</p>
      <template #footer>
        <el-button @click="configDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="configSaving" @click="submitConfig">保存</el-button>
      </template>
    </el-dialog>

    <!-- 渠道登记 -->
    <el-dialog v-model="channelDialogVisible" title="登记渠道" width="520px">
      <el-form label-position="top">
        <el-form-item label="渠道类型">
          <el-select v-model="channelDraft.kind">
            <el-option v-for="kind in RELEASE_CHANNEL_KINDS" :key="kind" :label="RELEASE_CHANNEL_KIND_LABELS[kind]" :value="kind" />
          </el-select>
        </el-form-item>
        <el-form-item label="渠道目标（目录条目 / 声明文件地址 / announce topic / 归档位置）">
          <el-input v-model="channelDraft.target" :maxlength="300" />
        </el-form-item>
        <el-form-item label="备注（可选）">
          <el-input v-model="channelDraft.note" :maxlength="500" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="channelDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="channelSaving" @click="submitChannel">登记</el-button>
      </template>
    </el-dialog>

    <!-- 发布详情 -->
    <el-dialog v-model="detailVisible" :title="detailTitle" width="720px">
      <template v-if="detailRelease">
        <div class="detail-tags">
          <el-tag size="small" :type="stateTagType(detailState)">{{ RELEASE_STATE_LABELS[detailState] }}</el-tag>
          <el-tag v-if="detailRelease.signature" size="small" :type="detailSignatureValid === true ? 'success' : detailSignatureValid === false ? 'danger' : 'info'">
            {{ detailSignatureValid === true ? '签名验讫' : detailSignatureValid === false ? '签名验签失败' : '签名待验' }}
          </el-tag>
          <el-tag v-if="detailRelease.updateManifest" size="small" type="info">含 update-manifest</el-tag>
        </div>

        <el-descriptions :column="1" border size="small" class="detail-desc">
          <el-descriptions-item label="目标插件">{{ detailRelease.pluginId }}</el-descriptions-item>
          <el-descriptions-item label="版本">v{{ detailRelease.version }}</el-descriptions-item>
          <el-descriptions-item label="发布者">{{ detailRelease.publisherRootId }}</el-descriptions-item>
          <el-descriptions-item label="登记时间">{{ formatDate(detailRelease.createdAt) }}</el-descriptions-item>
          <el-descriptions-item v-if="detailRelease.decisionRef" label="关联事务">{{ detailRelease.decisionRef }}</el-descriptions-item>
        </el-descriptions>

        <p v-if="detailRelease.changelog" class="detail-changelog">{{ detailRelease.changelog }}</p>

        <el-divider content-position="left">资产清单（{{ detailRelease.artifacts.length }}）</el-divider>
        <div v-for="asset in detailRelease.artifacts" :key="asset.fileName" class="artifact-row">
          <el-tag size="small">{{ asset.kind }}</el-tag>
          <span class="artifact-name">{{ asset.fileName }}</span>
          <span class="artifact-hash" :title="asset.sha256" @click="copyHash(asset.sha256)">{{ shortHash(asset.sha256) }}</span>
          <span>{{ asset.size }} B</span>
        </div>

        <el-divider content-position="left">存证状态</el-divider>
        <div class="evidence-row">
          <span>链头哈希：{{ evidence.headHash ? shortHash(evidence.headHash) : '（本机暂无入链记录）' }}</span>
          <el-tag v-if="evidence.chainValid !== null" size="small" :type="evidence.chainValid ? 'success' : 'danger'">
            链校验{{ evidence.chainValid ? `通过（高度 ${evidence.chainHeight}）` : '失败' }}
          </el-tag>
        </div>
        <p class="hint">
          发布单所在集合为 governance 语义（强制 append-only + 链式存证），包哈希随条目入链；锚定随内核锚定机制
          （治理事件驱动 + 每日兜底），本页不伪造「已锚定」。导出存证包 + 独立核验 CLI 离线核验请在桌面端操作
          （内核命令既有；移动端请在桌面端导出）。
        </p>

        <el-divider content-position="left">状态事件时间线（{{ detailTimeline.length }}）</el-divider>
        <p v-if="detailUnfilteredEventCount > 0" class="hint">
          其中 {{ detailUnfilteredEventCount }} 条事件的操作者不在发布权集合 ∪ 名册管理员内（或为配置未初始化期间的留痕），
          仅留痕展示、不参与状态派生。
        </p>
        <el-timeline v-if="detailTimeline.length > 0" class="event-timeline">
          <el-timeline-item v-for="event in detailTimeline" :key="event.id" :timestamp="formatDate(event.at)" placement="top">
            <div class="event-row">
              <el-tag size="small" :type="eventTagType(event.type)">{{ eventLabel(event.type) }}</el-tag>
              <span class="event-operator">操作者 {{ event.operatorRootId.slice(0, 12) }}…</span>
              <el-tag v-if="event.signature" size="small" type="success">已签名</el-tag>
            </div>
            <p v-if="event.reason" class="event-reason">{{ event.reason }}</p>
            <p v-if="event.channelId" class="event-detail">渠道 {{ channelLabelOf(event.channelId) }}<template v-if="event.resultRef"> · 结果引用 {{ event.resultRef }}</template></p>
            <p v-if="event.detail" class="event-detail">{{ event.detail }}</p>
            <template v-if="event.verification">
              <p class="event-detail">
                复算：{{ event.verification.recomputed ? shortHash(event.verification.recomputed.sha256) : '（导入失败）' }}
                · 双重核对资产 {{ event.verification.manifestAssetsChecked }} 条
                · 签名材料{{ event.verification.sigMaterialDeclared ? '已声明' : '未声明' }}
              </p>
              <p class="event-detail dim">{{ event.verification.trustNote }}</p>
            </template>
          </el-timeline-item>
        </el-timeline>
        <el-empty v-else description="尚无状态事件（已登记待核验）" :image-size="40" />

        <!-- 状态推进操作（发布权集合成员；业务层硬约束在服务层把关） -->
        <template v-if="canPublish">
          <el-divider content-position="left">状态推进</el-divider>
          <div v-if="detailState === 'registered' || detailState === 'verify-failed'" class="action-row">
            <el-input v-model="verifyDraft.spkgPath" size="small" placeholder=".spkg 包文件路径（本机导入复算）" class="verify-path" />
            <el-button v-if="marketAvailable" size="small" @click="pickSpkgFile">选择文件</el-button>
            <el-button size="small" type="primary" :loading="verifying" :disabled="!marketAvailable" @click="submitVerify">导入复算核验</el-button>
          </div>
          <p v-if="(detailState === 'registered' || detailState === 'verify-failed') && !marketAvailable" class="hint">
            当前环境无市场模块（移动端或桌面限定能力未授权）——只做登记，请委托桌面端成员核验（档二-8）。
          </p>
          <div v-if="detailState === 'verified'" class="action-row">
            <el-button size="small" type="primary" :loading="publishing" @click="submitPublish">推进「已发布」并推版本卡片</el-button>
          </div>
          <div v-if="detailState === 'published'" class="action-row">
            <el-select v-model="pushDraft.channelId" size="small" placeholder="选择渠道" class="push-channel">
              <el-option
                v-for="channel in channels"
                :key="channel.id"
                :label="`${RELEASE_CHANNEL_KIND_LABELS[channel.kind]} · ${channel.target}`"
                :value="channel.id"
              />
            </el-select>
            <el-input v-model="pushDraft.resultRef" size="small" placeholder="结果引用（可选）" class="push-ref" />
            <el-button size="small" :loading="pushing" :disabled="!pushDraft.channelId" @click="submitChannelPush">登记渠道推送</el-button>
          </div>
        </template>
        <div v-if="canRetract && detailState !== 'retracted'" class="action-row retract-row">
          <el-input v-model="retractDraft.reason" size="small" placeholder="撤回理由（可选，原样入链）" class="retract-reason" />
          <el-button size="small" type="danger" :loading="retracting" @click="submitRetract">撤回该版本</el-button>
        </div>
        <p v-if="detailState === 'retracted'" class="hint">已撤回：撤回记录 append-only 不可删改，已安装用户不回滚（档三-26）。</p>
      </template>
    </el-dialog>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginCardActionPayload, PluginMarketUpdateProbe, PluginSDK } from '../../packages/plugin-sdk/src';
import {
  canManageReleaseConfig,
  canPublishRelease,
  canRetractRelease,
  deriveReleaseState,
  deriveReleaseTimeline,
  deriveVersionDistribution,
  filterAuthorizedReleaseEvents,
  latestChannelPush,
  parseUpdateManifest,
  pluginDisplayName,
  releaseEventOperatorSet,
  RELEASE_CHANNEL_KIND_LABELS,
  RELEASE_CHANNEL_KINDS,
  RELEASE_STATE_LABELS,
  type ReleaseArtifact,
  type ReleaseChannel,
  type ReleaseChannelKind,
  type ReleaseEvent,
  type ReleaseEventType,
  type ReleaseManagerConfig,
  type ReleaseRecord,
  type ReleaseState,
  type VersionReport
} from './model';
import { ReleaseManagerService } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  members: Array<{ rootId: string; role: 'admin' | 'member' }>;
};

export default defineComponent({
  name: 'ReleaseManagerView',
  props: {
    /**
     * 运行上下文（桥握手 ctx 经入口注入；库包形态下由组合者注入）。
     * platform 供档二-8 降级判定（移动端只做登记）。
     */
    pluginContext: {
      type: Object as () => { spaceType?: 'personal' | 'org'; orgId?: string; platform?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<ReleaseManagerService | null>(null);
    const loading = ref(false);
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');

    const config = ref<ReleaseManagerConfig | null>(null);
    const configLoaded = ref(false);
    const releases = ref<ReleaseRecord[]>([]);
    const events = ref<ReleaseEvent[]>([]);
    const channels = ref<ReleaseChannel[]>([]);
    const reports = ref<VersionReport[]>([]);
    const updateProbes = ref<PluginMarketUpdateProbe[]>([]);
    const probeLoading = ref(false);
    const evidence = ref<{ headHash: string | null; chainValid: boolean | null; chainHeight: number | null }>({
      headHash: null,
      chainValid: null,
      chainHeight: null
    });

    const activeTab = ref<'releases' | 'channels'>('releases');
    const distributionPluginId = ref('');
    const reportDraft = ref({ version: '', trust: '' });
    const reportSaving = ref(false);

    // 登记向导
    const registerDialogVisible = ref(false);
    const registerSaving = ref(false);
    const registerDraft = ref({
      updateManifestJson: '',
      parsedArtifacts: [] as ReleaseArtifact[],
      pluginId: '',
      version: '',
      changelog: '',
      decisionRef: '',
      channels: [] as string[]
    });

    // 发布权配置
    const configDialogVisible = ref(false);
    const configSaving = ref(false);
    const configDraft = ref({ publisherRootIds: '' });

    // 渠道登记
    const channelDialogVisible = ref(false);
    const channelSaving = ref(false);
    const channelDraft = ref<{ kind: ReleaseChannelKind; target: string; note: string }>({
      kind: 'market-catalog',
      target: '',
      note: ''
    });

    // 详情
    const detailVisible = ref(false);
    const detailRelease = ref<ReleaseRecord | null>(null);
    const detailSignatureValid = ref<boolean | null>(null);
    const verifyDraft = ref({ spkgPath: '' });
    const verifying = ref(false);
    const publishing = ref(false);
    const pushDraft = ref({ channelId: '', resultRef: '' });
    const pushing = ref(false);
    const retractDraft = ref({ reason: '' });
    const retracting = ref(false);

    let offCardAction: (() => void) | null = null;

    const spaceReady = computed(() => Boolean(selectedOrgId.value));
    const isMobile = computed(() => props.pluginContext?.platform === 'android' || props.pluginContext?.platform === 'ios');
    const marketAvailable = computed(() => Boolean(service.value?.marketAvailable) && !isMobile.value);

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const isAdmin = computed(() => canManageReleaseConfig(currentOrgRole.value));
    const canPublish = computed(() => canPublishRelease(config.value, currentRootId.value));
    const canRetract = computed(() => canRetractRelease(config.value, currentRootId.value, currentOrgRole.value));

    /**
     * 事件读侧鉴权（伪造状态事件 fail-closed，spark-announcement 同范式）：
     * 合法操作者 = 发布权集合 ∪ 名册管理员；配置不可得时空集合 = 全部事件
     * 不参与派生（宁可不标状态、不推卡片），详情页时间线保留原始事件流兜底。
     */
    const eventOperatorIds = computed<ReadonlySet<string>>(() =>
      releaseEventOperatorSet(
        config.value,
        (activeOrg.value?.members ?? []).filter((member) => member.role === 'admin').map((member) => member.rootId)
      )
    );
    const authorizedEvents = computed(() => filterAuthorizedReleaseEvents(events.value, eventOperatorIds.value));

    const stateOf = (release: ReleaseRecord): ReleaseState => deriveReleaseState(release.id, authorizedEvents.value);

    const releaseRows = computed(() =>
      releases.value.map((release) => {
        const timeline = deriveReleaseTimeline(release.id, authorizedEvents.value);
        const latestFailure = [...timeline].reverse().find((event) => event.type === 'verify-failed');
        return {
          release,
          state: stateOf(release),
          failureReason: latestFailure?.reason ?? '',
          packageSha256: release.artifacts.find((asset) => asset.kind === 'package')?.sha256 ?? '',
          pushedChannelCount: new Set(
            timeline.filter((event) => event.type === 'channel-pushed').map((event) => event.channelId)
          ).size
        };
      })
    );

    const knownPluginIds = computed(() => [...new Set(releases.value.map((release) => release.pluginId))].sort());
    const distribution = computed(() =>
      distributionPluginId.value ? deriveVersionDistribution(distributionPluginId.value, reports.value) : null
    );

    const detailState = computed<ReleaseState>(() =>
      detailRelease.value ? deriveReleaseState(detailRelease.value.id, authorizedEvents.value) : 'registered'
    );
    // 详情页时间线保留原始事件流（含未通过读侧鉴权的留痕事件），并标注其不参与状态派生
    const detailTimeline = computed(() =>
      detailRelease.value ? deriveReleaseTimeline(detailRelease.value.id, events.value) : []
    );
    const detailUnfilteredEventCount = computed(() => {
      if (!detailRelease.value) {
        return 0;
      }
      return detailTimeline.value.filter((event) => !eventOperatorIds.value.has(event.operatorRootId)).length;
    });
    const detailTitle = computed(() =>
      detailRelease.value ? `${pluginDisplayName(detailRelease.value.pluginId)} v${detailRelease.value.version}` : '发布详情'
    );

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureSdk = async () => {
      if (!sdk.value) {
        sdk.value = await ensurePluginSDK();
        service.value = new ReleaseManagerService(sdk.value);
      }
      return sdk.value;
    };

    const loadOrganizations = async () => {
      const plugin = await ensureSdk();
      const all = await plugin.runtime.listMineOrganizations();
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

    /** 全量加载（配置 + 发布单 + 事件 + 渠道 + 上报 + 存证状态），并触发成员侧本地卡片补发 */
    const loadData = async () => {
      if (!service.value || !selectedOrgId.value) {
        return;
      }
      const orgId = selectedOrgId.value;
      const [loadedConfig, loadedReleases, loadedEvents, loadedChannels, loadedReports, loadedEvidence] = await Promise.all([
        service.value.loadConfig(orgId),
        service.value.loadReleases(orgId),
        service.value.loadEvents(orgId),
        service.value.loadChannels(orgId),
        service.value.loadVersionReports(orgId),
        service.value.getEvidenceStatus()
      ]);
      config.value = loadedConfig;
      configLoaded.value = true;
      releases.value = loadedReleases;
      events.value = loadedEvents;
      channels.value = loadedChannels;
      reports.value = loadedReports;
      evidence.value = loadedEvidence;
      if (!knownPluginIds.value.includes(distributionPluginId.value)) {
        distributionPluginId.value = knownPluginIds.value[0] ?? '';
      }
      // 成员侧「本地生成」版本卡片（档一-2 唯一推送源；插件加载时补发 + 节流，档二-4）
      // 读侧鉴权 fail-closed：配置不可得时传 null，宁可不补发（伪造事件不驱动卡片）
      try {
        await service.value.notifyNewReleases(
          orgId,
          loadedReleases,
          loadedEvents,
          loadedConfig ? eventOperatorIds.value : null
        );
      } catch (error) {
        console.warn('[spark-release-manager] 版本卡片补发失败（已降级）：', error);
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
        await loadData();
        // 同步后台化：先渲染本地数据，peer 同步成功后重载收敛
        void syncLatestFromPeers().then(async (synced) => {
          if (synced) {
            await loadData().catch(() => undefined);
          }
        });
      } catch (error) {
        setMessage(`加载失败：${error}`, 'error');
      } finally {
        loading.value = false;
      }
    };

    const onOrgChange = () => {
      void loadData().catch((error) => setMessage(`加载失败：${error}`, 'error'));
    };

    // ---------------- 登记向导 ----------------

    const openRegisterDialog = () => {
      registerDraft.value = {
        updateManifestJson: '',
        parsedArtifacts: [],
        pluginId: '',
        version: '',
        changelog: '',
        decisionRef: '',
        channels: []
      };
      registerDialogVisible.value = true;
    };

    /** 解析 update-manifest.json（视图层预览；服务层提交时会再解析校验一次，fail-closed） */
    const parseManifestDraft = () => {
      try {
        const parsed = parseUpdateManifest(registerDraft.value.updateManifestJson);
        registerDraft.value.parsedArtifacts = parsed.artifacts;
        if (parsed.manifest.pluginId && !registerDraft.value.pluginId) {
          registerDraft.value.pluginId = parsed.manifest.pluginId;
        }
        if (parsed.manifest.version && !registerDraft.value.version) {
          registerDraft.value.version = parsed.manifest.version;
        }
        ElMessage.success(`解析出 ${parsed.artifacts.length} 条资产，请人工核对后提交`);
      } catch (error) {
        ElMessage.error(`解析失败：${(error as Error).message}`);
      }
    };

    const submitRegister = async () => {
      registerSaving.value = true;
      try {
        if (!service.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.registerRelease(
          selectedOrgId.value,
          currentRootId.value,
          {
            pluginId: registerDraft.value.pluginId,
            version: registerDraft.value.version,
            ...(registerDraft.value.updateManifestJson.trim()
              ? { updateManifestJson: registerDraft.value.updateManifestJson }
              : { artifacts: registerDraft.value.parsedArtifacts }),
            changelog: registerDraft.value.changelog || undefined,
            decisionRef: registerDraft.value.decisionRef || undefined,
            channels: registerDraft.value.channels
          },
          config.value
        );
        registerDialogVisible.value = false;
        await loadData();
        setMessage('发布单已登记（签名入存证链，包哈希随之入链；待核验）', 'success');
      } catch (error) {
        setMessage(`登记失败：${(error as Error).message}`, 'error');
      } finally {
        registerSaving.value = false;
      }
    };

    // ---------------- 发布权配置 ----------------

    const submitConfig = async () => {
      configSaving.value = true;
      try {
        if (!service.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        const publisherRootIds = configDraft.value.publisherRootIds
          .split('\n')
          .map((item) => item.trim())
          .filter(Boolean);
        config.value = await service.value.saveConfig(
          selectedOrgId.value,
          currentRootId.value,
          { publisherRootIds },
          currentOrgRole.value
        );
        configDialogVisible.value = false;
        setMessage(`发布权配置已保存（${config.value.publisherRootIds.length} 名发布者）`, 'success');
      } catch (error) {
        setMessage(`配置保存失败：${(error as Error).message}`, 'error');
      } finally {
        configSaving.value = false;
      }
    };

    // ---------------- 渠道 ----------------

    const openChannelDialog = () => {
      channelDraft.value = { kind: 'market-catalog', target: '', note: '' };
      channelDialogVisible.value = true;
    };

    const submitChannel = async () => {
      channelSaving.value = true;
      try {
        if (!service.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.saveChannel(
          selectedOrgId.value,
          currentRootId.value,
          { kind: channelDraft.value.kind, target: channelDraft.value.target, note: channelDraft.value.note || undefined },
          config.value,
          currentOrgRole.value
        );
        channelDialogVisible.value = false;
        await loadData();
        setMessage('渠道已登记（声明性登记，不携带执行凭据）', 'success');
      } catch (error) {
        setMessage(`渠道登记失败：${(error as Error).message}`, 'error');
      } finally {
        channelSaving.value = false;
      }
    };

    const latestPushOf = (channelId: string) => latestChannelPush(channelId, events.value);
    const channelLabelOf = (channelId: string) => {
      const channel = channels.value.find((item) => item.id === channelId);
      return channel ? `${RELEASE_CHANNEL_KIND_LABELS[channel.kind]} · ${channel.target}` : channelId;
    };

    // ---------------- 详情与状态推进 ----------------

    const openDetail = async (releaseId: string) => {
      detailRelease.value = releases.value.find((release) => release.id === releaseId) ?? null;
      detailSignatureValid.value = null;
      verifyDraft.value = { spkgPath: '' };
      pushDraft.value = { channelId: '', resultRef: '' };
      retractDraft.value = { reason: '' };
      detailVisible.value = true;
      // 独立验签（验收⑤：sdk.identity.verify 重算比对；后台执行不阻塞展示）
      if (detailRelease.value?.signature && service.value) {
        const record = detailRelease.value;
        try {
          detailSignatureValid.value = await service.value.verifyReleaseSignature(record);
        } catch {
          detailSignatureValid.value = false;
        }
      }
    };

    const pickSpkgFile = async () => {
      const plugin = await ensureSdk();
      if (!plugin.market) {
        return;
      }
      try {
        const path = await plugin.market.pickSpkg();
        if (path) {
          verifyDraft.value.spkgPath = path;
        }
      } catch (error) {
        setMessage(`选择文件失败：${(error as Error).message}`, 'error');
      }
    };

    const submitVerify = async () => {
      verifying.value = true;
      try {
        if (!service.value || !currentRootId.value || !detailRelease.value) {
          throw new Error('Plugin service unavailable');
        }
        const event = await service.value.verifyRelease(
          selectedOrgId.value,
          currentRootId.value,
          detailRelease.value.id,
          { spkgPath: verifyDraft.value.spkgPath },
          config.value
        );
        await loadData();
        detailRelease.value = releases.value.find((release) => release.id === detailRelease.value?.id) ?? null;
        if (event.type === 'verified') {
          setMessage('核验通过：本机复算哈希/size 与登记值、update-manifest 三方一致（证据已入链）', 'success');
        } else {
          setMessage(`核验失败（原因已原样入链）：${event.reason ?? ''}`, 'error');
        }
      } catch (error) {
        setMessage(`核验执行失败：${(error as Error).message}`, 'error');
      } finally {
        verifying.value = false;
      }
    };

    const submitPublish = async () => {
      publishing.value = true;
      try {
        if (!service.value || !currentRootId.value || !detailRelease.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.publishRelease(selectedOrgId.value, currentRootId.value, detailRelease.value.id, config.value);
        await loadData();
        detailRelease.value = releases.value.find((release) => release.id === detailRelease.value?.id) ?? null;
        setMessage('已推进「已发布」：版本卡片已在本机生成（成员设备同步后各自本地生成，releaseRef 幂等去重）', 'success');
      } catch (error) {
        setMessage(`发布推进失败：${(error as Error).message}`, 'error');
      } finally {
        publishing.value = false;
      }
    };

    const submitChannelPush = async () => {
      pushing.value = true;
      try {
        if (!service.value || !currentRootId.value || !detailRelease.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.recordChannelPush(
          selectedOrgId.value,
          currentRootId.value,
          detailRelease.value.id,
          { channelId: pushDraft.value.channelId, resultRef: pushDraft.value.resultRef || undefined },
          config.value
        );
        pushDraft.value = { channelId: '', resultRef: '' };
        await loadData();
        setMessage('渠道推送已登记（append-only 留痕：渠道 + 时间 + 操作者签名 + 结果引用）', 'success');
      } catch (error) {
        setMessage(`渠道推送登记失败：${(error as Error).message}`, 'error');
      } finally {
        pushing.value = false;
      }
    };

    const submitRetract = async () => {
      retracting.value = true;
      try {
        if (!service.value || !currentRootId.value || !detailRelease.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.retractRelease(
          selectedOrgId.value,
          currentRootId.value,
          detailRelease.value.id,
          { reason: retractDraft.value.reason || undefined },
          config.value,
          currentOrgRole.value
        );
        await loadData();
        detailRelease.value = releases.value.find((release) => release.id === detailRelease.value?.id) ?? null;
        setMessage('已撤回：撤回记录 append-only 入链并推送告知卡片；已安装用户不回滚（档三-26）', 'success');
      } catch (error) {
        setMessage(`撤回失败：${(error as Error).message}`, 'error');
      } finally {
        retracting.value = false;
      }
    };

    // ---------------- 版本上报与更新探测 ----------------

    const submitReport = async () => {
      reportSaving.value = true;
      try {
        if (!service.value || !currentRootId.value || !distributionPluginId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.reportVersion(
          selectedOrgId.value,
          currentRootId.value,
          { pluginId: distributionPluginId.value, version: reportDraft.value.version, trust: reportDraft.value.trust },
          currentOrgRole.value
        );
        reportDraft.value = { version: '', trust: '' };
        await loadData();
        setMessage('已上报本机运行版本（opt-in；分布视图只显示计数，非全量）', 'success');
      } catch (error) {
        setMessage(`上报失败：${(error as Error).message}`, 'error');
      } finally {
        reportSaving.value = false;
      }
    };

    const runUpdateProbe = async () => {
      probeLoading.value = true;
      try {
        if (!service.value) {
          throw new Error('Plugin service unavailable');
        }
        const probes = await service.value.checkMarketUpdates();
        updateProbes.value = probes ?? [];
        if (!probes) {
          setMessage('市场模块不可用，无法探测更新', 'warning');
        }
      } catch (error) {
        setMessage(`更新探测失败：${(error as Error).message}`, 'error');
      } finally {
        probeLoading.value = false;
      }
    };

    // ---------------- 展示辅助 ----------------

    const shortHash = (hash: string) => (hash ? `${hash.slice(0, 12)}…${hash.slice(-6)}` : '—');
    const copyHash = async (hash: string) => {
      if (!hash) {
        return;
      }
      try {
        await navigator.clipboard.writeText(hash);
        ElMessage.success('包哈希已复制');
      } catch {
        ElMessage.info(hash);
      }
    };
    const formatDate = (ts: number) => new Date(ts).toLocaleString();
    const stateTagType = (state: ReleaseState) =>
      state === 'published'
        ? 'success'
        : state === 'verified'
          ? 'primary'
          : state === 'verify-failed'
            ? 'danger'
            : state === 'retracted'
              ? 'danger'
              : 'info';
    const eventTagType = (type: ReleaseEventType) =>
      type === 'verified' || type === 'published'
        ? 'success'
        : type === 'verify-failed' || type === 'retracted'
          ? 'danger'
          : 'info';
    const eventLabel = (type: ReleaseEventType) =>
      type === 'verified'
        ? '核验通过'
        : type === 'published'
          ? '已发布'
          : type === 'channel-pushed'
            ? '渠道推送'
            : type === 'verify-failed'
              ? '核验失败'
              : '已撤回';
    const distPercentage = (count: number) =>
      distribution.value && distribution.value.reporterCount > 0
        ? Math.round((count / distribution.value.reporterCount) * 100)
        : 0;

    const onCardAction = async (action: PluginCardActionPayload) => {
      // 卡片按钮回调（壳层归属校验后路由）：打开发布详情
      if (action.actionId !== 'goto-release') {
        return;
      }
      const data = action.data as { releaseId?: string; orgId?: string } | undefined;
      if (!data?.releaseId) {
        return;
      }
      if (data.orgId && data.orgId !== selectedOrgId.value && orgOptions.value.some((org) => org.orgId === data.orgId)) {
        selectedOrgId.value = data.orgId;
        await loadData();
      }
      activeTab.value = 'releases';
      openDetail(data.releaseId);
    };

    onMounted(async () => {
      await reloadAll();
      try {
        const plugin = await ensureSdk();
        if (plugin.messages) {
          offCardAction = plugin.messages.onCardAction((action) => {
            void onCardAction(action);
          });
        }
      } catch (error) {
        console.warn('[spark-release-manager] 卡片回调注册失败（降级）：', error);
      }
    });

    onUnmounted(() => {
      offCardAction?.();
    });

    return {
      loading,
      message,
      messageType,
      orgOptions,
      selectedOrgId,
      spaceReady,
      isMobile,
      marketAvailable,
      isAdmin,
      canPublish,
      canRetract,
      config,
      configLoaded,
      activeTab,
      releaseRows,
      channels,
      distributionPluginId,
      knownPluginIds,
      distribution,
      reportDraft,
      reportSaving,
      updateProbes,
      probeLoading,
      evidence,
      registerDialogVisible,
      registerSaving,
      registerDraft,
      configDialogVisible,
      configSaving,
      configDraft,
      channelDialogVisible,
      channelSaving,
      channelDraft,
      detailVisible,
      detailRelease,
      detailState,
      detailTimeline,
      detailUnfilteredEventCount,
      detailTitle,
      detailSignatureValid,
      verifyDraft,
      verifying,
      publishing,
      pushDraft,
      pushing,
      retractDraft,
      retracting,
      RELEASE_STATE_LABELS,
      RELEASE_CHANNEL_KINDS,
      RELEASE_CHANNEL_KIND_LABELS,
      pluginDisplayName,
      reloadAll,
      onOrgChange,
      openRegisterDialog,
      parseManifestDraft,
      submitRegister,
      submitConfig,
      openChannelDialog,
      submitChannel,
      latestPushOf,
      channelLabelOf,
      openDetail,
      pickSpkgFile,
      submitVerify,
      submitPublish,
      submitChannelPush,
      submitRetract,
      submitReport,
      runUpdateProbe,
      shortHash,
      copyHash,
      formatDate,
      stateTagType,
      eventTagType,
      eventLabel,
      distPercentage
    };
  }
});
</script>

<style scoped>
.spark-release-manager {
  padding: 12px;
  font-family: inherit;
}

.message {
  margin-bottom: 10px;
}

.header-card {
  margin-bottom: 10px;
}

.header-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}

.eyebrow {
  margin: 0;
  font-size: 12px;
  color: #0f766e;
  letter-spacing: 0.06em;
}

.header-row h2 {
  margin: 2px 0;
  font-size: 18px;
}

.lede {
  margin: 0;
  font-size: 12px;
  color: #64748b;
}

.selectors {
  margin-top: 10px;
  max-width: 360px;
}

.meta-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 8px;
}

.board-row {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

.main-tabs {
  margin-top: 4px;
}

.release-row {
  border: 1px solid #e2e8f0;
  border-radius: 8px;
  padding: 10px 12px;
  margin-bottom: 8px;
  cursor: pointer;
}

.release-row:hover {
  border-color: #0f766e;
}

.release-title-row {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}

.version {
  font-variant-numeric: tabular-nums;
  color: #334155;
}

.failure {
  margin: 6px 0 0;
  color: #b91c1c;
  font-size: 12px;
  word-break: break-all;
}

.release-meta {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
  margin-top: 6px;
  font-size: 12px;
  color: #64748b;
}

.hash {
  cursor: copy;
  font-variant-numeric: tabular-nums;
}

.panel-card {
  margin-bottom: 10px;
}

.honest-note {
  margin-left: 8px;
  font-size: 12px;
  color: #b45309;
}

.channel-row {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 6px 0;
  font-size: 13px;
}

.channel-target {
  flex: 1;
  word-break: break-all;
}

.channel-latest {
  color: #64748b;
  font-size: 12px;
}

.report-row {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 10px;
}

.report-plugin {
  min-width: 220px;
}

.report-version {
  max-width: 180px;
}

.report-trust {
  min-width: 190px;
}

.dist-row {
  display: flex;
  gap: 10px;
  align-items: center;
  margin-bottom: 6px;
}

.dist-version {
  min-width: 90px;
  font-variant-numeric: tabular-nums;
}

.dist-row :deep(.el-progress) {
  flex: 1;
}

.dist-count {
  min-width: 48px;
  text-align: right;
  color: #64748b;
  font-size: 12px;
}

.dist-trusts {
  margin-top: 8px;
  font-size: 12px;
  color: #64748b;
}

.dist-trust {
  margin-left: 6px;
}

.probe-row {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 6px 0;
  font-size: 13px;
}

.probe-reason {
  color: #64748b;
  font-size: 12px;
}

.artifact-row {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 4px 0;
  font-size: 12px;
}

.artifact-name {
  word-break: break-all;
}

.artifact-hash {
  color: #64748b;
  cursor: copy;
  font-variant-numeric: tabular-nums;
}

.hint {
  font-size: 12px;
  color: #64748b;
  line-height: 1.6;
}

.detail-tags {
  display: flex;
  gap: 8px;
  margin-bottom: 10px;
}

.detail-desc {
  margin-bottom: 10px;
}

.detail-changelog {
  white-space: pre-wrap;
  font-size: 13px;
  color: #334155;
  background: #f8fafc;
  border-radius: 6px;
  padding: 8px 10px;
}

.evidence-row {
  display: flex;
  gap: 10px;
  align-items: center;
  font-size: 12px;
  color: #334155;
}

.event-timeline {
  padding-left: 2px;
}

.event-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.event-operator {
  font-size: 12px;
  color: #64748b;
}

.event-reason {
  margin: 4px 0 0;
  color: #b91c1c;
  font-size: 12px;
  word-break: break-all;
}

.event-detail {
  margin: 4px 0 0;
  font-size: 12px;
  color: #475569;
}

.event-detail.dim {
  color: #94a3b8;
}

.action-row {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
}

.verify-path {
  flex: 1;
}

.push-channel {
  min-width: 220px;
}

.push-ref {
  flex: 1;
}

.retract-row {
  margin-top: 10px;
}

.retract-reason {
  flex: 1;
}
</style>
