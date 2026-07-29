<template>
  <div class="app-detail">
    <header class="app-detail-header">
      <el-button text :icon="ArrowLeft" @click="emit('back')">返回</el-button>
    </header>

    <section class="app-detail-hero">
      <span class="app-detail-icon" :style="{ background: appIconBackground(item) }">{{ item.name.slice(0, 1) }}</span>
      <div class="app-detail-hero-info">
        <h1>
          {{ item.name }}
          <el-tag v-if="item.updateAvailable" size="small" type="danger">可更新</el-tag>
          <el-tag v-if="item.installed && !enabled" size="small" type="warning">已禁用</el-tag>
        </h1>
        <p class="app-detail-meta">开发者 / 域名：{{ item.domain }}</p>
        <p class="app-detail-meta">
          版本：{{ item.installedVersion ?? item.version }}
          <template v-if="item.latestVersion && item.latestVersion !== item.installedVersion">
            （最新 {{ item.latestVersion }}）
          </template>
        </p>
        <p v-if="item.package.updateManifestUrl" class="app-detail-meta">
          更新清单：{{ item.package.updateManifestUrl }}
        </p>
      </div>
    </section>

    <section class="app-detail-section">
      <h2>应用简介</h2>
      <p>{{ item.description || '暂无简介' }}</p>
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

    <section class="app-detail-actions">
      <!-- 组织空间：只暴露启用/禁用（设计 §4.2），安装/卸载/更新由管理员统一管理 -->
      <template v-if="isOrgSpace">
        <el-button v-if="enabled" type="primary" @click="emit('open', item)">打开</el-button>
        <el-button
          v-if="!enabled"
          type="primary"
          :loading="busy === 'toggle'"
          @click="isAdmin ? emit('toggle', item) : emit('request-enable', item)"
        >
          启用
        </el-button>
        <el-button
          v-else-if="isAdmin"
          :loading="busy === 'toggle'"
          @click="emit('toggle', item)"
        >
          禁用
        </el-button>
        <el-button
          v-if="isAdmin && item.updateAvailable"
          type="warning"
          :loading="busy === 'upgrade'"
          @click="emit('upgrade', item)"
        >
          更新
        </el-button>
      </template>

      <!-- 个人空间：安装 / 启用 / 禁用 / 打开 / 更新（设计 §4.1/§4.3/§4.4） -->
      <template v-else>
        <el-button
          v-if="!item.installed"
          type="primary"
          :loading="busy === 'install'"
          @click="emit('install', item)"
        >
          安装
        </el-button>
        <template v-else>
          <el-button v-if="enabled" type="primary" @click="emit('open', item)">打开</el-button>
          <el-button :loading="busy === 'toggle'" @click="emit('toggle', item)">
            {{ enabled ? '禁用' : '启用' }}
          </el-button>
          <el-button
            v-if="item.updateAvailable"
            type="warning"
            :loading="busy === 'upgrade'"
            @click="emit('upgrade', item)"
          >
            更新
          </el-button>
        </template>
        <!-- TODO(mock): pluginMarket 无 uninstall 接口，暂不展示「卸载」按钮（设计 §4.1） -->
      </template>
    </section>
  </div>
</template>

<script lang="ts">
import { defineComponent, type PropType } from 'vue';
import { ArrowLeft } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { permissionLabel, appIconBackground } from './apps-store';

export default defineComponent({
  name: 'AppDetailPanel',
  props: {
    item: { type: Object as PropType<PluginMarketItemDto>, required: true },
    enabled: { type: Boolean, required: true },
    isOrgSpace: { type: Boolean, required: true },
    isAdmin: { type: Boolean, required: true },
    busy: { type: String, default: '' }
  },
  emits: ['back', 'open', 'install', 'upgrade', 'toggle', 'request-enable'],
  setup(props, { emit }) {
    // 模板中以 emit('xxx') 形式触发事件，必须把 emit 暴露出去（同 AppListPanel/AppMarketPanel）
    return { ArrowLeft, permissionLabel, appIconBackground, emit };
  }
});
</script>
