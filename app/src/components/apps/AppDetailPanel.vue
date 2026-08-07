<template>
  <div class="app-detail">
    <!-- 桌面端抽屉内返回（映射为关闭抽屉）；移动端由整页顶部 MobileBackBar 承担，不重复渲染 -->
    <header v-if="!isMobileLayout" class="app-detail-header">
      <el-button text :icon="ArrowLeft" @click="emit('back')">返回</el-button>
    </header>

    <section class="app-detail-hero">
      <div class="app-detail-hero-main">
        <span class="app-detail-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
        <div class="app-detail-hero-info">
          <h1>
            {{ item.name }}
            <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
            <el-tag v-if="item.installed && !enabled" size="small" type="warning">已禁用</el-tag>
          </h1>
          <!-- 开发者与域名拆成两个独立属性行：仓库锚定插件 id 为仓库地址（host/owner/repo），
               开发者取 owner 段；其余（域名签名插件）开发者即域名持有者 -->
          <p class="app-detail-meta">开发者：{{ developerText }}</p>
          <p class="app-detail-meta">域名：{{ item.domain }}</p>
          <p class="app-detail-meta">
            版本：{{ item.installedVersion ?? item.version }}
            <template v-if="item.latestVersion && item.latestVersion !== item.installedVersion">
              （最新 {{ item.latestVersion }}）
            </template>
          </p>
        </div>
      </div>

      <!-- 移动端：操作按钮收进「图标+名称」卡片内（图标名称在上、按钮组在下） -->
      <div v-if="isMobileLayout" class="app-detail-hero-actions">
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
      </div>
    </section>

    <section class="app-detail-section">
      <h2>应用简介</h2>
      <p>{{ item.description || '暂无简介' }}</p>
      <!-- 更新清单并入简介卡片展示（原 hero 属性行） -->
      <p v-if="item.package.updateManifestUrl" class="app-detail-meta">
        更新清单：{{ item.package.updateManifestUrl }}
      </p>
    </section>

    <section class="app-detail-section">
      <h2>所需权限</h2>
      <ul v-if="item.permissions.length > 0" class="app-detail-permissions">
        <li v-for="permission in item.permissions" :key="permission">
          <span class="permission-check">✓</span>
          {{ permissionLabel(permission) }}
          <span class="permission-code">{{ permission }}</span>
        </li>
      </ul>
      <p v-else class="app-detail-muted">该应用未声明额外权限</p>
    </section>

    <section class="app-detail-section">
      <h2>签名与来源</h2>
      <p class="app-detail-meta">应用域名：{{ item.domain }}</p>
      <p v-if="item.package.signatureUrl" class="app-detail-meta">
        签名地址：{{ item.package.signatureUrl }}
      </p>
      <!-- TODO(mock): 市场数据暂无源码仓库地址与 Ed25519 域名签名详情（设计 §3.4/§6.2），待内核补充字段后展示 -->
      <p class="app-detail-muted">源码仓库与签名指纹信息暂未提供</p>
    </section>

    <!-- 桌面端：操作按钮区在详情页底部（移动端已收进 hero 卡片，此处不渲染） -->
    <section v-if="!isMobileLayout" class="app-detail-actions">
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
import { ArrowLeft } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { isMobileLayout } from '../../stores/ui-layout';
import { permissionLabel, appIconBackground } from './apps-store';
import AppDetailActions from './AppDetailActions.vue';

export default defineComponent({
  name: 'AppDetailPanel',
  components: { AppDetailActions },
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
     *  其余插件开发者即域名持有者（市场数据暂无独立开发者字段，设计 §3.4） */
    const developerText = computed(() => {
      const segments = props.item.id.split('/');
      return segments.length >= 3 ? segments[1] : props.item.domain;
    });
    // 模板中以 emit('xxx') 形式触发事件，必须把 emit 暴露出去（同 AppListPanel/AppMarketPanel）
    return { ArrowLeft, permissionLabel, appIconBackground, developerText, isMobileLayout, emit };
  }
});
</script>
