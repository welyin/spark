<template>
  <section class="rootid-page">
    <header class="page-header">
      <div class="page-header-main">
        <p class="eyebrow">身份凭证</p>
        <h1>RootID</h1>
        <p class="lede">RootID 是用户唯一的本地底层主权凭证，所有域身份、签名、授权都由它派生签发。</p>
      </div>
      <div class="page-header-actions">
        <el-button text type="primary" @click="emit('back')">&lt; 返回我的</el-button>
        <el-button @click="refreshStatus">刷新状态</el-button>
        <el-button type="danger" plain @click="handleLogout">退出登录</el-button>
      </div>
    </header>

    <el-row :gutter="12" class="info-grid">
      <el-col :md="8" :sm="24">
        <el-card shadow="never" class="info-card">
          <h2>域身份派生</h2>
          <p>从 RootID 按 SLIP-0010 Ed25519 硬化路径派生出域身份，用于插件、组织、应用等不同域。</p>
          <p class="muted">功能：为不同域生成独立公钥与域标识，避免共享根私钥。</p>
        </el-card>
      </el-col>
      <el-col :md="8" :sm="24">
        <el-card shadow="never" class="info-card">
          <h2>RootID 签名</h2>
          <p>对修改数据、跨域授权和治理操作进行 Ed25519 签名，主进程可校验签名真实性。</p>
          <p class="muted">功能：证明操作由当前登录的 RootID 所属用户发起。</p>
        </el-card>
      </el-col>
      <el-col :md="8" :sm="24">
        <el-card shadow="never" class="info-card">
          <h2>助记词</h2>
          <p>24 个汉字助记词是 RootID 的最终恢复凭证，注册时展示一次，之后可在"我的 → 身份备份"中验证密码后再次查看。</p>
          <p class="muted">功能：设备丢失或迁移时恢复 RootID（需用户自行妥善离线保存）。</p>
        </el-card>
      </el-col>
    </el-row>

    <el-card shadow="never" class="ops-card">
      <template #header>
        <h2>RootID 相关信息</h2>
      </template>

      <el-descriptions :column="1" border>
        <el-descriptions-item label="是否已注册">{{ rootStatus.initialized ? '是' : '否' }}</el-descriptions-item>
        <el-descriptions-item label="是否已登录">{{ rootStatus.unlocked ? '是' : '否' }}</el-descriptions-item>
        <el-descriptions-item label="RootID">{{ rootStatus.rootId || '未创建' }}</el-descriptions-item>
      </el-descriptions>

      <el-row :gutter="14" class="split">
        <el-col :md="12" :sm="24">
          <el-card shadow="never">
            <template #header>
              <h3>域身份派生</h3>
            </template>
            <el-form label-position="top">
              <el-form-item label="域名">
                <el-input v-model="domainInput" placeholder="plugin:weibo-core" />
              </el-form-item>
              <el-button type="primary" @click="deriveDomainIdentity">派生域身份</el-button>
            </el-form>
            <div v-if="derivedDomain" class="mono-box">
              <div><strong>domain:</strong> {{ derivedDomain.domain }}</div>
              <div><strong>domainId:</strong> {{ derivedDomain.domainId }}</div>
              <div><strong>publicKey:</strong> {{ derivedDomain.publicKey }}</div>
              <div><strong>path:</strong> {{ derivedDomain.derivationPath }}</div>
            </div>
          </el-card>
        </el-col>

        <el-col :md="12" :sm="24">
          <el-card shadow="never">
            <template #header>
              <h3>RootID 签名</h3>
            </template>
            <el-form label-position="top">
              <el-form-item label="待签名文本">
                <el-input v-model="payloadInput" placeholder="hello-root-id" />
              </el-form-item>
              <el-button type="primary" @click="signPayload">签名</el-button>
            </el-form>
            <div v-if="signatureResult" class="mono-box">
              <div><strong>rootId:</strong> {{ signatureResult.rootId }}</div>
              <div><strong>payloadHash:</strong> {{ signatureResult.payloadHash }}</div>
              <div><strong>signature:</strong> {{ signatureResult.signature }}</div>
            </div>
          </el-card>
        </el-col>
      </el-row>
    </el-card>

    <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon />
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';

type RootStatus = {
  initialized: boolean;
  unlocked: boolean;
  rootId: string | null;
};

type DerivedDomain = {
  domain: string;
  domainId: string;
  publicKey: string;
  derivationPath: string;
};

type SignatureResult = {
  rootId: string;
  signature: string;
  payloadHash: string;
};

export default defineComponent({
  name: 'RootIDPage',
  emits: ['logout', 'back'],
  setup(_, { emit }) {
    const rootStatus = ref<RootStatus>({ initialized: false, unlocked: false, rootId: null });
    const message = ref('');
    const domainInput = ref('plugin:weibo-core');
    const derivedDomain = ref<DerivedDomain | null>(null);
    const payloadInput = ref('hello-root-id');
    const signatureResult = ref<SignatureResult | null>(null);

    const refreshStatus = async () => {
      rootStatus.value = await window.electronAPI.rootIdentity.status();
    };

    const handleLogout = async () => {
      try {
        await window.electronAPI.rootIdentity.lock();
        message.value = '已退出登录';
        derivedDomain.value = null;
        signatureResult.value = null;
        await refreshStatus();
        emit('logout');
        ElMessage.success(message.value);
      } catch (error) {
        message.value = `退出失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    const deriveDomainIdentity = async () => {
      try {
        derivedDomain.value = await window.electronAPI.rootIdentity.deriveDomain(domainInput.value.trim());
        message.value = `已派生域身份：${derivedDomain.value.domain}`;
        ElMessage.success(message.value);
      } catch (error) {
        message.value = `派生失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    const signPayload = async () => {
      try {
        signatureResult.value = await window.electronAPI.rootIdentity.sign(payloadInput.value);
        message.value = '签名成功';
        ElMessage.success(message.value);
      } catch (error) {
        message.value = `签名失败：${error}`;
        ElMessage.error(message.value);
      }
    };

    onMounted(async () => {
      try {
        await refreshStatus();
      } catch (error) {
        message.value = `读取状态失败：${error}`;
      }
    });

    return {
      rootStatus,
      message,
      domainInput,
      derivedDomain,
      payloadInput,
      signatureResult,
      refreshStatus,
      handleLogout,
      deriveDomainIdentity,
      signPayload,
      emit
    };
  }
});
</script>

<style scoped src="../styles/pages/root-id.css"></style>
