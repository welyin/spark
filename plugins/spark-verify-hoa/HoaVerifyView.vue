<!--
  业主资格验证示例（spark-verify-hoa）· 主视图。

  两个角色同页（验证插件是方法，验证人才是信任——同一插件服务双方）：
  - 申请人：选验证方式（组织加入规则声明的组合）、填户号、附材料，提交申请；
  - 验证人：看申请摘要（材料原文只在申请人为本机的本地留存，审核以线下/
    加密通道核对为实）、签发演示级凭证（identity:sign）、对已发凭证注销；
  - 凭证：本插件签发的演示凭证只做「演示级自查」；内核持有的协议凭证经
    sdk.credentials 只读面（listHeld / presentHolderProof / queryVerifiers /
    verify / queryRevocations）查询、出示、内核验证链裁决与注销快照查询——
    错误如实呈现，不按权限降级静默吞掉。

  诚实口径（评审阻塞 3/4，README 同步）：本插件签发的凭证签名主体是插件域
  身份，不证明验证人个人身份；issuerRootId 为自报文本；HoaCredential 线形
  与内核凭证体系零互操作（演示级数据）。

  窗口化适配（ui-architecture §4.2，窗口最小夹取 320×220）：申请人/验证人/
  凭证三侧经 el-tabs 分页（非双栏），窄窗天然单列堆叠，tabs 头溢出由
  Element Plus 自带滚动箭头承载；材料行/操作行 flex-wrap 换行、输入框弹性
  收缩，材料哈希与自报 rootId 任意断行（overflow-wrap）；根节点不设
  height/overflow，超高申请/凭证列表由 iframe 原生文档滚动承载。
-->
<template>
  <section class="spark-verify-hoa">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">验证插件示例（业主场景）</p>
          <h2>业主资格验证</h2>
          <p class="lede">验证插件是方法，验证人才是信任。凭证只暴露资格结论（户号），不含姓名证件号。</p>
        </div>
        <el-button @click="reload" :loading="loading">刷新</el-button>
      </div>
      <el-form label-position="top" class="selectors" v-if="orgOptions.length > 0">
        <el-form-item label="小区组织">
          <el-select v-model="selectedOrgId" @change="reload" placeholder="选择组织">
            <el-option v-for="org in orgOptions" :key="org.orgId" :label="org.name" :value="org.orgId" />
          </el-select>
        </el-form-item>
      </el-form>
      <el-empty v-if="orgOptions.length === 0" description="你还没有加入任何组织。" />
    </el-card>

    <template v-if="selectedOrgId">
      <el-card shadow="never">
        <el-tabs>
          <!-- ── 申请人侧 ── -->
          <el-tab-pane label="申请人：提交材料">
            <el-form label-position="top">
              <el-form-item label="验证方式（组织加入规则声明接受的组合）">
                <el-select v-model="applyForm.method">
                  <el-option label="房产证人工核验（deed-manual）" value="deed-manual" />
                  <el-option label="两户在册业主担保（vouch-2）" value="vouch-2" />
                  <el-option label="政务实名接口（gov-realname）" value="gov-realname" />
                </el-select>
              </el-form-item>
              <el-form-item label="凭证类型">
                <el-radio-group v-model="applyForm.credentialType">
                  <el-radio-button value="owner">业主凭证</el-radio-button>
                  <el-radio-button value="resident">居住凭证（租户等）</el-radio-button>
                </el-radio-group>
              </el-form-item>
              <el-form-item label="户号（凭证只暴露到这个粒度）">
                <el-input v-model="applyForm.unitNo" maxlength="20" placeholder="如 3-502" />
              </el-form-item>
              <el-form-item label="材料（原件只给验证人看，不进公共数据、不上链）">
                <div v-for="(material, index) in applyForm.materials" :key="index" class="material-row">
                  <el-input v-model="material.label" maxlength="40" placeholder="材料名称（如 房产证照片）" class="material-label" />
                  <el-input v-model="material.content" type="textarea" :rows="2" placeholder="材料内容（示例中以文本代替实际文件）" />
                  <el-button size="small" text type="danger" @click="applyForm.materials.splice(index, 1)">移除</el-button>
                </div>
                <el-button size="small" text @click="applyForm.materials.push({ label: '', content: '' })">
                  + 添加材料
                </el-button>
              </el-form-item>
              <div class="actions">
                <el-button type="primary" :loading="submitting" @click="submitApplication">提交申请</el-button>
              </div>
            </el-form>
          </el-tab-pane>

          <!-- ── 验证人侧 ── -->
          <el-tab-pane label="验证人：审核签发">
            <el-alert type="warning" :closable="false" show-icon class="message"
              title="签发的是演示级凭证：签名主体为插件域身份（plugin:spark-verify-hoa），密码学上不证明验证人个人身份；签发人 rootId 为自报文本；线形不与内核凭证体系互操作（详见插件 README）。" />
            <p class="hint">申请列表只含材料摘要（标签 + 哈希 + 大小）；材料原文留在申请人本机，请按线下/加密通道核对后签发。</p>
            <el-empty v-if="applications.length === 0" description="暂无申请" />
            <div v-for="application in applications" :key="application.applicationId" class="record-item">
              <div class="record-meta">
                <strong>{{ application.unitNo }}</strong>
                <el-tag size="small" :type="application.credentialType === 'owner' ? 'primary' : 'info'">
                  {{ application.credentialType === 'owner' ? '业主' : '居住' }}
                </el-tag>
                <el-tag size="small" type="warning">{{ methodText(application.method) }}</el-tag>
                <el-tag size="small" :type="statusTagType(statusOf(application))">{{ statusText(statusOf(application)) }}</el-tag>
                <span class="record-origin">申请人（自报）{{ application.applicantRootId }} · {{ formatDate(application.createdAt) }}</span>
              </div>
              <p class="hint" v-for="material in application.materials" :key="material.contentHash">
                材料：{{ material.label }} · {{ material.byteSize }}B · 哈希 {{ material.contentHash }}
              </p>
              <div class="actions" v-if="statusOf(application) === 'pending'">
                <el-button size="small" type="primary" :loading="issuingId === application.applicationId"
                  @click="issue(application)">签发演示凭证</el-button>
              </div>
            </div>

            <el-divider />
            <h4>已签发凭证（演示级线形）</h4>
            <el-empty v-if="credentials.length === 0" description="暂无凭证" />
            <div v-for="credential in credentials" :key="credential.credentialId" class="record-item">
              <div class="record-meta">
                <strong>{{ credential.unitNo }}</strong>
                <el-tag size="small" :type="credential.credentialType === 'owner' ? 'primary' : 'info'">
                  {{ credential.credentialType === 'owner' ? '业主' : '居住' }}
                </el-tag>
                <el-tag v-if="isRevoked(credential)" size="small" type="danger">已注销</el-tag>
                <span class="record-origin">签发人（自报）{{ credential.issuerRootId }} · {{ formatDate(credential.issuedAt) }}</span>
              </div>
              <div class="actions" v-if="!isRevoked(credential)">
                <el-input v-model="revokeReasons[credential.credentialId]" size="small" placeholder="注销理由（资格变更）" class="revoke-input" />
                <el-button size="small" type="danger" plain :loading="revokingId === credential.credentialId"
                  @click="revoke(credential)">注销</el-button>
              </div>
            </div>
          </el-tab-pane>

          <!-- ── 凭证 ── -->
          <el-tab-pane label="凭证：自查 / 内核凭证面">
            <el-alert type="info" :closable="false" show-icon class="message"
              title="本插件签发的凭证为演示级数据：不进 cred:held: 键域、不过内核验证链；「演示自查」只重算载荷 + 验插件域签名 + 查本地注销表，不核对签发人信任链。下方「内核凭证面」才是与内核凭证体系互操作的只读接口（credentials:read）。" />

            <h4>演示凭证（本插件签发，演示级）</h4>
            <el-empty v-if="credentials.length === 0" description="暂无凭证" />
            <div v-for="credential in credentials" :key="credential.credentialId" class="record-item">
              <div class="record-meta">
                <strong>{{ credential.unitNo }}</strong>
                <el-tag v-if="isRevoked(credential)" size="small" type="danger">已注销</el-tag>
              </div>
              <div class="actions">
                <el-button size="small" :loading="verifyingId === credential.credentialId" @click="verify(credential)">演示自查</el-button>
              </div>
              <p class="hint" v-if="verifyResults[credential.credentialId]">
                自查结果：{{ verifyResults[credential.credentialId] }}
              </p>
            </div>

            <el-divider />
            <h4>内核凭证面（sdk.credentials 只读）</h4>
            <template v-if="credentialsAvailable">
              <el-empty v-if="heldCredentials.length === 0" description="本机未持有协议凭证（cred:held: 键域为空）" />
              <div v-for="held in heldCredentials" :key="held.credId" class="record-item">
                <div class="record-meta">
                  <strong>{{ held.credential.credType }}</strong>
                  <el-tag size="small" type="info">{{ held.credential.method }}</el-tag>
                  <span class="record-origin">
                    签发人 {{ shortId(held.credential.issuer.identity) }} · credId {{ shortId(held.credId) }}
                  </span>
                </div>
                <div class="actions kernel-verify-row">
                  <el-button size="small" :loading="kernelVerifyingId === held.credId"
                    @click="kernelVerify(held)">内核验证</el-button>
                  <el-button size="small" text @click="revocationIssuer = held.credential.issuer.identity">
                    查签发人注销快照
                  </el-button>
                </div>
                <p class="hint" v-if="kernelVerifyResults[held.credId]">
                  内核裁决：{{ kernelVerifyResults[held.credId] }}
                </p>
              </div>
              <el-form label-position="top" class="selectors" v-if="heldCredentials.length > 0">
                <el-form-item label="出示 holderProof（read-gate §3；域身份由桥注入，插件不可自报）">
                  <div class="material-row">
                    <el-select v-model="presentForm.credId" placeholder="选择持有的凭证" class="material-label">
                      <el-option v-for="held in heldCredentials" :key="held.credId" :label="`${held.credential.credType} · ${shortId(held.credId)}`" :value="held.credId" />
                    </el-select>
                    <el-input v-model="presentForm.collection" placeholder="集合（如 members）" class="material-label" />
                    <el-input v-model="presentForm.requestId" placeholder="请求 id" class="material-label" />
                    <el-button size="small" type="primary" :loading="presenting" @click="presentHeld">出示</el-button>
                  </div>
                </el-form-item>
              </el-form>
              <div class="actions verifier-row">
                <el-button size="small" :loading="queryingVerifiers" @click="loadVerifiers">查询本组织验证人信任声明</el-button>
              </div>
              <p class="hint" v-if="verifiersInfo">{{ verifiersInfo }}</p>
              <el-form label-position="top" class="selectors">
                <el-form-item label="注销快照查询（sdk.credentials.queryRevocations；按签发人身份 id，快照缺失如实报「不可用」，不冒充无注销）">
                  <div class="material-row">
                    <el-input v-model="revocationIssuer" placeholder="签发人身份 id（64 位小写 hex）" class="material-label" />
                    <el-button size="small" :loading="queryingRevocations" @click="queryRevocations">查询</el-button>
                  </div>
                </el-form-item>
              </el-form>
              <p class="hint" v-if="revocationInfo">{{ revocationInfo }}</p>
            </template>
            <el-alert v-else type="warning" :closable="false" show-icon
              title="当前宿主未提供可用的 sdk.credentials（资格凭证 SDK 模块），内核凭证面不可用。" />
          </el-tab-pane>
        </el-tabs>
      </el-card>
    </template>
  </section>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue';
import { ensurePluginSDK, type HeldCredential, type PluginSDK } from '../../packages/plugin-sdk/src';
import { HoaVerifyService } from './service';
import { deriveApplicationStatus, type ApplicationStatus, type CredentialRevocation, type HoaCredential, type VerificationApplication, type VerificationMethod } from './model';

const message = ref('');
const messageType = ref<'success' | 'error'>('success');
const loading = ref(false);
const submitting = ref(false);
const issuingId = ref<string | null>(null);
const revokingId = ref<string | null>(null);
const verifyingId = ref<string | null>(null);
const kernelVerifyingId = ref<string | null>(null);
const presenting = ref(false);
const queryingVerifiers = ref(false);
const queryingRevocations = ref(false);
const orgOptions = ref<Array<{ orgId: string; name: string }>>([]);
const selectedOrgId = ref('');
const applications = ref<VerificationApplication[]>([]);
const credentials = ref<HoaCredential[]>([]);
const heldCredentials = ref<HeldCredential[]>([]);
const revokeReasons = reactive<Record<string, string>>({});
const verifyResults = reactive<Record<string, string>>({});
const kernelVerifyResults = reactive<Record<string, string>>({});
const credentialsAvailable = ref(false);
const verifiersInfo = ref('');
const revocationIssuer = ref('');
const revocationInfo = ref('');
const presentForm = reactive({ credId: '', collection: 'members', requestId: `req-${Date.now()}` });

const applyForm = reactive<{
  method: VerificationMethod;
  credentialType: 'owner' | 'resident';
  unitNo: string;
  materials: Array<{ label: string; content: string }>;
}>({
  method: 'deed-manual',
  credentialType: 'owner',
  unitNo: '',
  materials: [{ label: '', content: '' }]
});

let sdk: PluginSDK;
let service: HoaVerifyService;

function show(text: string, type: 'success' | 'error'): void {
  message.value = text;
  messageType.value = type;
}

function formatDate(ts: number): string {
  return new Date(ts).toLocaleString();
}

function shortId(identity: string): string {
  return identity.length > 16 ? `${identity.slice(0, 12)}…` : identity;
}

function methodText(method: VerificationMethod): string {
  return { 'deed-manual': '房产证人工核验', 'vouch-2': '两户担保', 'gov-realname': '政务实名' }[method];
}

function statusOf(application: VerificationApplication): ApplicationStatus {
  return deriveApplicationStatus(application, credentials.value, revokedList.value as never);
}

function statusText(status: ApplicationStatus): string {
  return { pending: '待审核', issued: '已签发', revoked: '已注销' }[status];
}

function statusTagType(status: ApplicationStatus): 'warning' | 'success' | 'danger' {
  return { pending: 'warning', issued: 'success', revoked: 'danger' }[status];
}

function isRevoked(credential: HoaCredential): boolean {
  const application = applications.value.find((item) => item.applicationId === credential.applicationId);
  if (!application) {
    return false;
  }
  return deriveApplicationStatus(application, [credential], revokedList.value) === 'revoked';
}

// 注销状态：本地从凭证/注销集合推导（演示级口径，与内核注销链无关）
const revokedList = ref<CredentialRevocation[]>([]);

async function reload(): Promise<void> {
  if (!selectedOrgId.value) {
    return;
  }
  loading.value = true;
  try {
    const [apps, creds, revocations] = await Promise.all([
      service.listApplications(selectedOrgId.value),
      service.listCredentials(selectedOrgId.value),
      service.listRevocations()
    ]);
    applications.value = apps;
    credentials.value = creds;
    revokedList.value = revocations;
    // 内核凭证面独立加载：不可用/未授权只影响该分区，如实提示
    if (credentialsAvailable.value) {
      try {
        heldCredentials.value = await service.listHeldCredentials();
      } catch (error) {
        heldCredentials.value = [];
        show(`持有凭证查询失败：${(error as Error).message}`, 'error');
      }
    }
  } catch (error) {
    show(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
  }
}

async function submitApplication(): Promise<void> {
  submitting.value = true;
  try {
    const root = await sdk.runtime.currentRoot();
    await service.submitApplication(selectedOrgId.value, root.rootId ?? 'unknown', {
      method: applyForm.method,
      credentialType: applyForm.credentialType,
      unitNo: applyForm.unitNo,
      materials: applyForm.materials
    });
    show('申请已提交。材料原文仅留存本机，摘要随申请同步给验证人。', 'success');
    applyForm.unitNo = '';
    applyForm.materials = [{ label: '', content: '' }];
    await reload();
  } catch (error) {
    show(`提交失败：${(error as Error).message}`, 'error');
  } finally {
    submitting.value = false;
  }
}

async function issue(application: VerificationApplication): Promise<void> {
  issuingId.value = application.applicationId;
  try {
    const root = await sdk.runtime.currentRoot();
    const credential = await service.issueCredential(application, root.rootId ?? 'unknown');
    show(`演示凭证 ${credential.credentialId.slice(0, 12)}… 已签发（${credential.unitNo}；签名主体为插件域身份）。`, 'success');
    await reload();
  } catch (error) {
    show(`签发失败：${(error as Error).message}`, 'error');
  } finally {
    issuingId.value = null;
  }
}

async function revoke(credential: HoaCredential): Promise<void> {
  revokingId.value = credential.credentialId;
  try {
    const root = await sdk.runtime.currentRoot();
    await service.revokeCredential(credential, revokeReasons[credential.credentialId] ?? '', root.rootId ?? 'unknown');
    show('凭证已注销（演示级记录，历史原样保留）。', 'success');
    await reload();
  } catch (error) {
    show(`注销失败：${(error as Error).message}`, 'error');
  } finally {
    revokingId.value = null;
  }
}

async function presentHeld(): Promise<void> {
  if (!presentForm.credId) {
    show('请选择要出示的持有凭证。', 'error');
    return;
  }
  presenting.value = true;
  try {
    // 出示失败（权限拒绝/内核校验失败）如实上抛呈现，不做权限降级式静默
    const result = await service.presentHeldProof({
      credId: presentForm.credId,
      requestId: presentForm.requestId,
      orgId: selectedOrgId.value,
      collection: presentForm.collection
    });
    show(`holderProof 已出示（credId ${shortId(result.holderProof.credId)}，${formatDate(result.presentedAt)}）。`, 'success');
  } catch (error) {
    show(`出示失败：${(error as Error).message}`, 'error');
  } finally {
    presenting.value = false;
  }
}

async function loadVerifiers(): Promise<void> {
  queryingVerifiers.value = true;
  try {
    const verifiers = await service.queryVerifiers(selectedOrgId.value);
    verifiersInfo.value =
      verifiers.verifiers.length === 0
        ? '该组织暂无验证人信任声明（空集）。'
        : `验证人 ${verifiers.verifiers.length} 名（seq ${verifiers.seq}）：` +
          verifiers.verifiers
            .map((grant) => `${shortId(grant.identity)} 可签发 ${grant.credTypes.join('/')}`)
            .join('；');
  } catch (error) {
    verifiersInfo.value = `查询失败：${(error as Error).message}`;
  } finally {
    queryingVerifiers.value = false;
  }
}

async function verify(credential: HoaCredential): Promise<void> {
  verifyingId.value = credential.credentialId;
  try {
    const result = await service.verifyCredential(credential);
    verifyResults[credential.credentialId] = result.valid
      ? `通过（演示级自查${result.reason ? `：${result.reason}` : ''}）`
      : `未通过（${result.reason ?? '原因未知'}）`;
  } catch (error) {
    verifyResults[credential.credentialId] = `自查失败：${(error as Error).message}`;
  } finally {
    verifyingId.value = null;
  }
}

// 内核验证链（credential §6 第 1–5 步结构化裁决）：只服务协议线形凭证；
// 逐项结果如实回显，不做「权限降级」式静默
async function kernelVerify(held: HeldCredential): Promise<void> {
  kernelVerifyingId.value = held.credId;
  try {
    const result = await service.verifyProtocolCredential(held.credential);
    const checks = `静态链 ${result.checks.static ? '过' : '不过'} · 信任链 ${result.checks.trust ? '匹配' : '不匹配'} · 注销检查 ${result.checks.revocation}`;
    kernelVerifyResults[held.credId] = result.valid
      ? `有效（${checks}）`
      : `无效（${checks}${result.reason ? `；首个失败段：${result.reason}` : ''}）`;
  } catch (error) {
    kernelVerifyResults[held.credId] = `验证调用失败：${(error as Error).message}`;
  } finally {
    kernelVerifyingId.value = null;
  }
}

// 注销快照查询：available:false = 本机无快照（注销状态未知），不得冒充「无注销」
async function queryRevocations(): Promise<void> {
  const issuer = revocationIssuer.value.trim();
  if (!issuer) {
    show('请填入签发人身份 id。', 'error');
    return;
  }
  queryingRevocations.value = true;
  try {
    const snapshot = await service.queryRevocationSnapshot(issuer);
    revocationInfo.value = snapshot.available
      ? `快照可用：head seq ${snapshot.headSeq}，${snapshot.entries.length} 条注销记录（asOf ${formatDate(snapshot.asOf)}）。`
      : '本机无该签发人的注销快照（available:false：注销状态未知，不等于无注销）。';
  } catch (error) {
    revocationInfo.value = `查询失败：${(error as Error).message}`;
  } finally {
    queryingRevocations.value = false;
  }
}

onMounted(async () => {
  try {
    sdk = await ensurePluginSDK();
  } catch {
    show('插件 SDK 注入超时：不在插件运行上下文。', 'error');
    return;
  }
  credentialsAvailable.value = HoaVerifyService.isAvailable(sdk);
  service = new HoaVerifyService(sdk);
  try {
    const orgs = await sdk.runtime.listMineOrganizations();
    orgOptions.value = orgs.map((org) => ({ orgId: org.orgId, name: org.name }));
    if (orgs.length > 0) {
      selectedOrgId.value = orgs[0].orgId;
      await reload();
    }
  } catch (error) {
    show(`初始化失败：${(error as Error).message}`, 'error');
  }
});
</script>

<style scoped>
.spark-verify-hoa {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.message {
  margin-bottom: 4px;
}
.header-card h2 {
  margin: 0 0 4px;
}
.eyebrow {
  margin: 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.lede {
  margin: 0;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 8px;
}
.selectors {
  margin-top: 8px;
}
/* 材料哈希（64 位 hex）/自报 rootId/内核裁决串是无空格长文本：任意断行不横向溢出 */
.hint {
  margin: 6px 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  overflow-wrap: anywhere;
}
/* 窄窗（窗口最小宽 320）下注销输入框 + 按钮等一行放不下时换行，不横向挤出卡片 */
.actions {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 8px;
}
.verifier-row {
  justify-content: flex-start;
}
.kernel-verify-row {
  justify-content: flex-start;
}
/* 窄窗换行堆叠：材料名 + 内容 + 移除钮（及出示行的三字段 + 钮）单行在
   320–480 档必然溢出 */
.material-row {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin-bottom: 8px;
  width: 100%;
  flex-wrap: wrap;
}
/* 字段弹性收缩（min-width:0 否则 flex 项按内容最小宽撑开）：宽窗受
   max-width 限制，窄窗换行后独占一行 */
.material-label {
  flex: 1 1 200px;
  min-width: 0;
  max-width: 220px;
}
/* 材料内容 textarea 随容器弹性伸缩（同理 min-width:0 防撑开溢出） */
.material-row .el-textarea {
  flex: 1 1 200px;
  min-width: 0;
}
.record-item {
  padding: 10px 0;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.record-item:last-child {
  border-bottom: none;
}
.record-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
/* 申请人/签发人 rootId 为自报文本（无空格长字符串）：任意断行，窄窗不横向溢出 */
.record-origin {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  overflow-wrap: anywhere;
}
/* 输入框弹性收缩（min-width:0 否则 flex 项按内容最小宽撑开）：
   宽窗受 max-width 限制右对齐，窄窗换行后独占一行占满可用宽 */
.revoke-input {
  flex: 1;
  min-width: 0;
  max-width: 260px;
  margin-left: auto;
}
</style>
