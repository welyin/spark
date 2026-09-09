<!--
  担保链门槛示例（spark-threshold-vouch）· 主视图。

  三段流程同页呈现：发起请求（被担保人）→ 签名担保（N 名已有参与者）→
  组装门槛证明（任一持有者可组装）。证明列表提供免权限验签，展示逐项
  检查结论——「内核只验证产物」的演示闭环。

  诚实口径（评审阻塞 3）：所有担保/组装签名的主体是插件域身份
  （plugin:spark-threshold-vouch），密码学上不证明担保人/组装人个人身份；
  担保人 rootId 为自报文本。验签通过 = 载荷完整且插件域签名有效，验的不是个人。
-->
<template>
  <section class="spark-threshold-vouch">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">门槛插件示例（担保链）</p>
          <h2>担保链门槛</h2>
          <p class="lede">N 名已有参与者签名担保 → 产出「是否满足门槛」的签名证明；事务客户端只验证证明产物。</p>
        </div>
        <el-button @click="reload" :loading="loading">刷新</el-button>
      </div>
      <el-alert type="warning" :closable="false" show-icon class="message"
        title="演示级实现：担保与组装签名均以本插件域身份出具，不证明担保人/组装人个人身份；担保人 rootId 为自报文本，恶意用户可为任意「担保人」伪造担保且验签通过（详见插件 README）。" />
    </el-card>

    <el-card shadow="never" class="composer-card">
      <template #header>
        <h3>发起担保请求</h3>
      </template>
      <el-form label-position="top">
        <el-form-item label="治理上下文（如事务 id，证明只在此上下文内有效）">
          <el-input v-model="draft.context" maxlength="120" placeholder="affair_xxx / 项目空间标识" />
        </el-form-item>
        <el-form-item label="被担保人">
          <el-input v-model="draft.subjectRootId" placeholder="被担保人 rootId" />
        </el-form-item>
        <el-form-item label="门槛：需要几名已有参与者担保">
          <el-input-number v-model="draft.requiredCount" :min="1" :max="100" />
        </el-form-item>
        <el-form-item label="说明">
          <el-input v-model="draft.note" type="textarea" :rows="2" maxlength="200" show-word-limit
            placeholder="为什么需要担保、担保人应具备什么资格（业务语义在插件，协议无地位）" />
        </el-form-item>
        <div class="actions">
          <el-button type="primary" :loading="creating" @click="createRequest">发起请求</el-button>
        </div>
      </el-form>
    </el-card>

    <el-card shadow="never">
      <template #header>
        <h3>担保请求</h3>
      </template>
      <el-empty v-if="requests.length === 0" description="暂无请求" />
      <div v-for="request in requests" :key="request.requestId" class="request-item">
        <div class="request-meta">
          <strong>{{ request.context }}</strong>
          <el-tag size="small" :type="metMap[request.requestId] ? 'success' : 'info'">
            {{ vouchCountMap[request.requestId] ?? 0 }}/{{ request.requiredCount }} 份担保
          </el-tag>
        </div>
        <p class="hint">被担保人（自报）{{ request.subjectRootId }} · {{ request.note || '（无说明）' }}</p>
        <div class="actions">
          <el-input v-model="voucherInputs[request.requestId]" size="small" placeholder="担保人 rootId（自报文本）" class="voucher-input" />
          <el-button size="small" type="primary" :loading="vouchingId === request.requestId" @click="vouch(request)">
            签名担保（插件域身份）
          </el-button>
          <el-button size="small" :disabled="!metMap[request.requestId]" :loading="assemblingId === request.requestId"
            @click="assemble(request)">
            组装门槛证明
          </el-button>
        </div>
      </div>
    </el-card>

    <el-card shadow="never">
      <template #header>
        <h3>门槛证明（演示产物：验的是插件域签名与载荷完整性，不证明担保人个人身份）</h3>
      </template>
      <el-empty v-if="proofs.length === 0" description="暂无证明" />
      <div v-for="proof in proofs" :key="proof.proofId" class="request-item">
        <div class="request-meta">
          <strong>{{ proof.context }}</strong>
          <el-tag size="small" type="primary">{{ proof.vouches.length }} 份担保 / 门槛 {{ proof.requiredCount }}</el-tag>
          <el-tag size="small" :type="proofResults[proof.proofId]?.valid ? 'success' : 'info'">
            {{ proofResults[proof.proofId] ? (proofResults[proof.proofId].valid ? '验证通过' : '验证未过') : '未验证' }}
          </el-tag>
        </div>
        <p class="hint">
          被担保人（自报）{{ proof.subjectRootId }} · 组装人（自报）{{ proof.assembledBy }} · {{ formatDate(proof.assembledAt) }}
        </p>
        <div class="actions">
          <el-button size="small" :loading="verifyingId === proof.proofId" @click="verify(proof)">验签（免权限，验插件域签名）</el-button>
        </div>
        <ul v-if="proofResults[proof.proofId]" class="checks">
          <li v-for="check in proofResults[proof.proofId].checks" :key="check.name">
            <el-tag size="small" :type="check.ok ? 'success' : 'danger'">{{ check.ok ? '通过' : '未过' }}</el-tag>
            {{ check.name }}<template v-if="check.detail">（{{ check.detail }}）</template>
          </li>
        </ul>
      </div>
    </el-card>
  </section>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue';
import { ensurePluginSDK, type PluginSDK } from '../../packages/plugin-sdk/src';
import { VouchService } from './service';
import { isThresholdMet, countDistinctVouchers, type ThresholdProof, type VouchRequest } from './model';

const message = ref('');
const messageType = ref<'success' | 'error'>('success');
const loading = ref(false);
const creating = ref(false);
const vouchingId = ref<string | null>(null);
const assemblingId = ref<string | null>(null);
const verifyingId = ref<string | null>(null);
const requests = ref<VouchRequest[]>([]);
const proofs = ref<ThresholdProof[]>([]);
const vouchCountMap = reactive<Record<string, number>>({});
const metMap = reactive<Record<string, boolean>>({});
const voucherInputs = reactive<Record<string, string>>({});
const proofResults = reactive<Record<string, { valid: boolean; checks: Array<{ name: string; ok: boolean; detail?: string }> }>>({});

const draft = reactive({
  context: '',
  subjectRootId: '',
  requiredCount: 2,
  note: ''
});

let sdk: PluginSDK;
let service: VouchService;

function show(text: string, type: 'success' | 'error'): void {
  message.value = text;
  messageType.value = type;
}

function formatDate(ts: number): string {
  return new Date(ts).toLocaleString();
}

async function refreshCounts(): Promise<void> {
  for (const request of requests.value) {
    const distinct = await service.listDistinctVouches(request.requestId);
    vouchCountMap[request.requestId] = countDistinctVouchers(distinct);
    metMap[request.requestId] = isThresholdMet(distinct, request.requiredCount, request.subjectRootId);
  }
}

async function reload(): Promise<void> {
  loading.value = true;
  try {
    requests.value = await service.listRequests();
    proofs.value = await service.listProofs();
    await refreshCounts();
  } catch (error) {
    show(`加载失败：${(error as Error).message}`, 'error');
  } finally {
    loading.value = false;
  }
}

async function createRequest(): Promise<void> {
  creating.value = true;
  try {
    await service.createRequest({ ...draft });
    show('担保请求已发起。', 'success');
    draft.note = '';
    await reload();
  } catch (error) {
    show(`发起失败：${(error as Error).message}`, 'error');
  } finally {
    creating.value = false;
  }
}

async function vouch(request: VouchRequest): Promise<void> {
  const voucher = voucherInputs[request.requestId]?.trim();
  if (!voucher) {
    show('请填写担保人 rootId。', 'error');
    return;
  }
  vouchingId.value = request.requestId;
  try {
    await service.addVouch(request, voucher);
    show('担保签名已记录（插件域身份签名，担保人 rootId 为自报文本）。', 'success');
    voucherInputs[request.requestId] = '';
    await reload();
  } catch (error) {
    show(`担保失败：${(error as Error).message}`, 'error');
  } finally {
    vouchingId.value = null;
  }
}

async function assemble(request: VouchRequest): Promise<void> {
  assemblingId.value = request.requestId;
  try {
    // assembledBy 取本机根身份 id——自报文本（签名主体是插件域身份，不证明个人身份）
    const root = await sdk.runtime.currentRoot();
    const proof = await service.assembleProof(request, root.rootId ?? 'unknown');
    show(`门槛证明 ${proof.proofId.slice(0, 12)}… 已组装。`, 'success');
    await reload();
  } catch (error) {
    show(`组装失败：${(error as Error).message}`, 'error');
  } finally {
    assemblingId.value = null;
  }
}

async function verify(proof: ThresholdProof): Promise<void> {
  verifyingId.value = proof.proofId;
  try {
    proofResults[proof.proofId] = await service.verifyProof(proof);
  } catch (error) {
    show(`验证失败：${(error as Error).message}`, 'error');
  } finally {
    verifyingId.value = null;
  }
}

onMounted(async () => {
  try {
    sdk = await ensurePluginSDK();
  } catch {
    show('插件 SDK 注入超时：不在插件运行上下文。', 'error');
    return;
  }
  service = new VouchService(sdk);
  await reload();
});
</script>

<style scoped>
.spark-threshold-vouch {
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
.composer-card h3,
h3 {
  margin: 0;
}
/* 窄窗（窗口最小宽 320）下担保输入框 + 两个按钮一行放不下时换行，不横向挤出卡片 */
.actions {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 8px;
}
/* 输入框弹性收缩（min-width:0 否则 flex 项按内容最小宽撑开）：
   宽窗受 max-width 限制右对齐，窄窗换行后独占一行占满可用宽 */
.voucher-input {
  flex: 1;
  min-width: 0;
  max-width: 240px;
  margin-left: auto;
}
.request-item {
  padding: 10px 0;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.request-item:last-child {
  border-bottom: none;
}
.request-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
/* 治理上下文是用户输入的长字符串（可达 120 字符无空格）：任意断行不横向溢出 */
.request-meta strong {
  overflow-wrap: anywhere;
}
/* rootId（自报文本）是无空格长字符串：任意断行，窄窗不横向溢出 */
.hint {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  overflow-wrap: anywhere;
}
.checks {
  margin: 8px 0 0;
  padding-left: 0;
  list-style: none;
  font-size: 12px;
  color: var(--el-text-color-regular);
}
/* 检查项名含担保人 rootId 长字符串（如「担保签名 root-xxx…」）：任意断行不溢出 */
.checks li {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 4px;
  overflow-wrap: anywhere;
}
</style>
