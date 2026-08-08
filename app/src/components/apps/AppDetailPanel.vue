<template>
  <div class="app-detail">
    <!-- 桌面端抽屉内返回（映射为关闭抽屉）；移动端由整页顶部 MobileBackBar 承担，不重复渲染 -->
    <header v-if="!isMobileLayout" class="app-detail-header">
      <el-button text :icon="ArrowLeft" @click="emit('back')">返回</el-button>
      <button type="button" class="app-detail-header-close" title="关闭" @click="emit('back')">
        <el-icon :size="16"><Close /></el-icon>
      </button>
    </header>

    <!-- 中间可滚动内容区 -->
    <div class="app-detail-body">
      <section class="app-detail-hero">
        <span class="app-detail-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
        <div class="app-detail-hero-info">
          <h1>{{ item.name }}</h1>
          <div class="app-detail-status">
            <!-- 未安装条目无启用状态，仅标注未安装；已安装才区分启用/禁用 -->
            <span class="status-dot" :class="!item.installed ? '' : enabled ? 'status-dot--on' : 'status-dot--off'" />
            <span class="status-text">{{ !item.installed ? '未安装' : enabled ? '已启用' : '已禁用' }}</span>
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
        <a
          v-if="item.package.updateManifestUrl"
          class="app-detail-update-link"
          :href="item.package.updateManifestUrl"
          target="_blank"
          rel="noopener noreferrer"
          @click.prevent="openExternal(item.package.updateManifestUrl)"
        >
          查看更新清单
          <el-icon :size="12"><TopRight /></el-icon>
        </a>
      </section>

      <section class="app-detail-section">
        <div class="app-detail-section-title">
          <h2>所需权限</h2>
          <span class="app-detail-section-subtitle">
            声明 {{ item.permissions.length }} 项权限
          </span>
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
          <span
            class="verified-badge"
            :class="hasSignature ? 'verified-badge--ok' : 'verified-badge--warn'"
          >
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

    <!-- 操作按钮区：桌面端在内容底部；移动端固定在屏幕底部 -->
    <section class="app-detail-actions">
      <AppDetailActions
        :item="item"
        :enabled="enabled"
        :is-org-space="isOrgSpace"
        :is-admin="isAdmin"
        :busy="busy"
        @open="emit('open', item)"
        @install="emit('install', item)"
        @upgrade="emit('upgrade', item)"
        @toggle="emit('toggle', item)"
        @uninstall="emit('uninstall', item)"
        @request-enable="emit('request-enable', item)"
      />
    </section>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ArrowLeft, Check, CircleCheck, Close, CopyDocument, TopRight, WarningFilled } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { isMobileLayout } from '../../stores/ui-layout';
import { permissionLabel, appIconBackground } from './apps-store';
import AppDetailActions from './AppDetailActions.vue';

export default defineComponent({
  name: 'AppDetailPanel',
  components: { AppDetailActions, ArrowLeft, Check, CircleCheck, Close, CopyDocument, TopRight, WarningFilled },
  props: {
    item: { type: Object as PropType<PluginMarketItemDto>, required: true },
    enabled: { type: Boolean, required: true },
    isOrgSpace: { type: Boolean, required: true },
    isAdmin: { type: Boolean, required: true },
    busy: { type: String, default: '' }
  },
  emits: ['back', 'open', 'install', 'upgrade', 'toggle', 'uninstall', 'request-enable'],
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

    /** 打开外部链接（更新清单/签名地址等） */
    const openExternal = (url: string) => {
      window.open(url, '_blank', 'noopener,noreferrer');
    };

    /** 复制文本到剪贴板 */
    const copyText = async (text: string) => {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        // 复制失败静默处理，避免阻断用户体验
      }
    };

    return {
      ArrowLeft,
      Check,
      CircleCheck,
      Close,
      CopyDocument,
      TopRight,
      WarningFilled,
      permissionLabel,
      appIconBackground,
      developerText,
      sourceRepoUrl,
      hasSignature,
      openExternal,
      copyText,
      isMobileLayout,
      emit
    };
  }
});
</script>
