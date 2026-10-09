<!-- 应用详情面板（系统层，移植自壳层 AppDetailPanel 的 system 模式）：
     英雄区（图标/名称/安装态/开发者/域名/版本）+ 简介 + 所需权限 + 签名与来源，
     底部操作条（安装/更新/卸载）。
     插件版裁剪（桥面没有对应数据面，见任务报告遗留）：
     - 「空间启用情况」分区（壳层 per-space app-enablement 事实源）不迁移；
     - 更新清单/源码仓库链接在沙箱 iframe 内不可开窗，呈现为可复制文本。 -->
<template>
  <div class="app-detail">
    <header class="app-detail-header">
      <el-button text :icon="ArrowLeft" @click="emit('back')">返回</el-button>
      <button type="button" class="app-detail-header-close" title="关闭" @click="emit('back')">
        <el-icon :size="16"><Close /></el-icon>
      </button>
    </header>

    <div class="app-detail-body">
      <section class="app-detail-hero">
        <AppIcon class="app-detail-icon" :item="item" />
        <div class="app-detail-hero-info">
          <h1>{{ item.name }}</h1>
          <div class="app-detail-status">
            <span class="status-dot" :class="item.installed ? 'status-dot--on' : ''" />
            <span class="status-text">{{ item.installed ? '已安装' : '未安装' }}</span>
            <!-- 信任级展示（A34：community-model §十 安装通路信任级） -->
            <el-tag v-if="trustLabel" size="small" effect="plain" class="app-detail-trust">{{ trustLabel }}</el-tag>
          </div>
          <div class="app-detail-meta-list">
            <div class="app-detail-meta-row">
              <span class="app-detail-meta-label">开发者</span>
              <span class="app-detail-meta-value">{{ developerText }}</span>
            </div>
            <div class="app-detail-meta-row">
              <span class="app-detail-meta-label">域名</span>
              <span class="app-detail-meta-value">{{ item.domain }}</span>
            </div>
            <div class="app-detail-meta-row">
              <span class="app-detail-meta-label">版本</span>
              <span class="app-detail-meta-value">
                {{ item.installedVersion ?? item.version }}
                <template v-if="item.latestVersion && item.latestVersion !== item.installedVersion">
                  （最新 {{ item.latestVersion }}）
                </template>
              </span>
            </div>
          </div>
        </div>
      </section>

      <section class="app-detail-section">
        <h2>应用简介</h2>
        <p class="app-detail-desc">{{ item.description || '暂无简介' }}</p>
      </section>

      <section class="app-detail-section">
        <div class="app-detail-section-title">
          <h2>所需权限</h2>
          <span class="app-detail-section-subtitle">声明 {{ item.permissions.length }} 项权限</span>
        </div>
        <ul v-if="item.permissions.length > 0" class="app-detail-permissions">
          <li v-for="permission in item.permissions" :key="permission">
            <el-icon class="permission-check" :size="16"><Check /></el-icon>
            <span class="permission-name">{{ permissionLabel(permission) }}</span>
            <span class="permission-code">{{ permission }}</span>
          </li>
        </ul>
        <p v-else class="app-detail-muted">该应用未声明额外权限</p>
      </section>

      <section class="app-detail-section">
        <div class="app-detail-section-title">
          <h2>签名与来源</h2>
          <span class="verified-badge" :class="hasSignature ? 'verified-badge--ok' : 'verified-badge--warn'">
            <el-icon :size="12"><CircleCheck v-if="hasSignature" /><WarningFilled v-else /></el-icon>
            {{ hasSignature ? '已提供签名' : '未提供签名' }}
          </span>
        </div>
        <div class="app-detail-source-list">
          <div class="app-source-row">
            <span class="app-source-label">应用域名</span>
            <span class="app-source-value">{{ item.domain }}</span>
            <button type="button" class="app-source-copy" title="复制" @click="copyText(item.domain)">
              <el-icon :size="14"><CopyDocument /></el-icon>
            </button>
          </div>
          <div v-if="item.package.updateManifestUrl" class="app-source-row">
            <span class="app-source-label">更新清单</span>
            <span class="app-source-value">{{ item.package.updateManifestUrl }}</span>
            <button type="button" class="app-source-copy" title="复制" @click="copyText(item.package.updateManifestUrl)">
              <el-icon :size="14"><CopyDocument /></el-icon>
            </button>
          </div>
          <div v-if="hasSignature" class="app-source-row">
            <span class="app-source-label">签名地址</span>
            <span class="app-source-value">{{ item.package.signatureUrl }}</span>
            <button type="button" class="app-source-copy" title="复制" @click="copyText(item.package.signatureUrl)">
              <el-icon :size="14"><CopyDocument /></el-icon>
            </button>
          </div>
          <div v-if="sourceRepoUrl" class="app-source-row">
            <span class="app-source-label">源码仓库</span>
            <span class="app-source-value">{{ sourceRepoUrl }}</span>
            <button type="button" class="app-source-copy" title="复制" @click="copyText(sourceRepoUrl)">
              <el-icon :size="14"><CopyDocument /></el-icon>
            </button>
          </div>
        </div>
        <p class="app-detail-source-note" :class="hasSignature ? '' : 'app-detail-source-note--warn'">
          <el-icon :size="12"><CircleCheck v-if="hasSignature" /><WarningFilled v-else /></el-icon>
          {{ hasSignature ? '已提供签名地址，安装时将进行签名校验。' : '该应用未提供签名，来源未经核验。' }}
        </p>
      </section>
    </div>

    <section class="app-detail-actions">
      <AppDetailActions
        :item="item"
        :busy="busy"
        @install="emit('install', item)"
        @upgrade="emit('upgrade', item)"
        @uninstall="emit('uninstall', item)"
      />
    </section>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { ArrowLeft, Check, CircleCheck, Close, CopyDocument, WarningFilled } from '@element-plus/icons-vue';
import type { PluginMarketItem } from '../../../../packages/plugin-sdk/src';
import { permissionLabel } from '../market-store';
import AppDetailActions from './AppDetailActions.vue';
import AppIcon from './AppIcon.vue';

export default defineComponent({
  name: 'AppDetailPanel',
  components: { AppDetailActions, AppIcon, ArrowLeft, Check, CircleCheck, Close, CopyDocument, WarningFilled },
  props: {
    item: { type: Object as PropType<PluginMarketItem>, required: true },
    busy: { type: String, default: '' }
  },
  emits: ['back', 'install', 'upgrade', 'uninstall'],
  setup(props, { emit }) {
    /** 开发者展示：仓库锚定插件（id 形如 host/owner/repo）取仓库 owner 段；
     *  其余（域名签名插件）开发者即域名持有者（设计 §3.4） */
    const developerText = computed(() => {
      const segments = props.item.id.split('/');
      return segments.length >= 3 ? segments[1] : props.item.domain;
    });

    /** 源码仓库地址推导：id 形如 host/owner/repo 时还原为 https://host/owner/repo */
    const sourceRepoUrl = computed(() => {
      const segments = props.item.id.split('/');
      if (segments.length >= 3) {
        const [host, owner, repo, ...rest] = segments;
        return `https://${host}/${owner}/${repo}${rest.length ? '/' + rest.join('/') : ''}`;
      }
      return '';
    });

    /** 是否提供了签名（签名地址非空视为已签名） */
    const hasSignature = computed(() => Boolean(props.item.package.signatureUrl));

    /** 信任级展示（community-model §十：L2 签名 / L1 仓库锚定 / L0 侧载·内置） */
    const trustLabel = computed(() => {
      switch (props.item.trustLevel) {
        case 'L2':
          return '签名信任';
        case 'L1':
          return '仓库锚定';
        case 'L0':
          return '侧载/内置';
        default:
          return '';
      }
    });

    /** 复制文本到剪贴板（沙箱 iframe 可能无剪贴板权限，失败如实提示） */
    const copyText = async (text: string) => {
      try {
        await navigator.clipboard.writeText(text);
        ElMessage.success('已复制');
      } catch {
        ElMessage.info(`复制不可用，请手动复制：${text}`);
      }
    };

    return {
      ArrowLeft,
      permissionLabel,
      developerText,
      sourceRepoUrl,
      hasSignature,
      trustLabel,
      copyText,
      emit
    };
  }
});
</script>
