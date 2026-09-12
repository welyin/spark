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
        <AppIcon class="app-detail-icon" :item="item" />
        <div class="app-detail-hero-info">
          <h1>{{ item.name }}</h1>
          <div class="app-detail-status">
            <!-- 系统层详情：状态只反映安装与否（启用是空间层关系，见下方「空间启用情况」） -->
            <span class="status-dot" :class="item.installed ? 'status-dot--on' : ''" />
            <span class="status-text">{{ item.installed ? '已安装' : '未安装' }}</span>
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
          <h2>空间启用情况</h2>
          <span class="app-detail-section-subtitle">安装是系统层本机动作；启用按空间各自生效</span>
        </div>
        <!-- 只列出已启用的空间（走查修正：未启用的不列）；一个都没有时如实显示 -->
        <ul v-if="enabledSpaceRows.length > 0" class="app-detail-spaces">
          <li
            v-for="row in enabledSpaceRows"
            :key="row.key"
            class="app-detail-space-row"
            :class="{ 'app-detail-space-row--current': row.isCurrent }"
          >
            <span class="app-detail-space-name">
              {{ row.name }}<template v-if="row.isCurrent">（当前空间）</template>
            </span>
            <span class="app-detail-space-side">
              <span v-if="row.usedRecently" class="app-detail-space-used">最近使用</span>
              <el-tag size="small" effect="plain" type="success">已启用</el-tag>
            </span>
          </li>
        </ul>
        <p v-else class="app-detail-muted">未在任何空间启用</p>
        <!-- 过渡期诚实标注（install-and-enable §五）：组织启用清单应由内核接管
             （变更走组织事务、随组织副本全体一致同步），落地前本机生效、成员间不同步 -->
        <p class="app-detail-space-note">
          组织空间的启用/停用当前仅在本机生效，成员间同步待内核「组织启用清单」落地；规则类应用的正式启用需走组织事务决议。启用/停留在列表行的当前空间开关进行（空间层动作），本页不操作。
        </p>
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

    <!-- 操作按钮区：system＝系统层（仅安装/更新/卸载）；space＝空间的应用市场（仅启用/停用）；
         桌面端在内容底部，移动端固定在屏幕底部 -->
    <section class="app-detail-actions">
      <AppDetailActions
        v-if="mode === 'system'"
        :item="item"
        :busy="busy"
        @install="emit('install', item)"
        @upgrade="emit('upgrade', item)"
        @uninstall="emit('uninstall', item)"
      />
      <div v-else class="app-detail-action-bar">
        <!-- 空间层唯一操作：当前空间的启用/停用（启用＝逻辑状态，不要求代码在场；
             未安装的应用首次打开时由插件宿主提示就地安装） -->
        <button
          type="button"
          class="action-btn action-btn--primary action-btn--full"
          :disabled="toggleBusy || !canToggleHere"
          @click="onToggleHere"
        >
          <el-icon :size="18"><SwitchButton /></el-icon>
          <span>{{ enabledHere ? '在本空间停用' : item.installed ? '在本空间启用' : '在本空间启用（首次打开时提示安装）' }}</span>
        </button>
        <p v-if="!canToggleHere" class="action-hint">启用需走组织流程（过渡期由管理员在本机操作）</p>
      </div>
    </section>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ArrowLeft, Check, CircleCheck, Close, CopyDocument, SwitchButton, TopRight, WarningFilled } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../../api/types';
import { isMobileLayout } from '../../stores/ui-layout';
import { currentSpace } from '../../stores/current-space';
import {
  isAppEnabledInSpace,
  isAppEnableableInSpace,
  type EnablementSpace
} from '../../stores/app-enablement';
import { isAdmin, organizations } from '../../stores/org-membership';
import { toggleAppEnablement } from './app-actions';
import { personalSpaceName } from '../../stores/personal-space';
import { permissionLabel } from './apps-store';
import AppDetailActions from './AppDetailActions.vue';
import AppIcon from './AppIcon.vue';

/** 「最近使用」记录键（apps-store useRecentApps 同口径：spark:apps-recent:<bareSpaceKey>，best-effort 读取） */
function recentIdsOf(spaceKey: string): string[] {
  try {
    const raw = localStorage.getItem(`spark:apps-recent:${spaceKey}`);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

export default defineComponent({
  name: 'AppDetailPanel',
  components: { AppDetailActions, AppIcon, ArrowLeft, Check, CircleCheck, Close, CopyDocument, SwitchButton, TopRight, WarningFilled },
  props: {
    item: { type: Object as PropType<PluginMarketItemDto>, required: true },
    busy: { type: String, default: '' },
    /** system＝系统层应用管理（装卸）；space＝空间的应用市场（启用/停用） */
    mode: { type: String as PropType<'system' | 'space'>, default: 'system' }
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

    /** 空间模式：当前空间的启用状态与启停操作（组织空间非管理员只读） */
    const enableSpaceHere = computed<EnablementSpace>(() =>
      currentSpace.value.type === 'org'
        ? { type: 'org', orgId: currentSpace.value.orgId }
        : { type: 'personal' }
    );
    const enabledHere = computed(() => isAppEnabledInSpace(enableSpaceHere.value, props.item));
    const canToggleHere = computed(() =>
      currentSpace.value.type === 'org'
        ? isAdmin(currentSpace.value.orgId) && isAppEnableableInSpace(enableSpaceHere.value, props.item)
        : isAppEnableableInSpace(enableSpaceHere.value, props.item)
    );
    const toggleBusy = ref(false);
    const onToggleHere = async () => {
      if (toggleBusy.value) {
        return;
      }
      toggleBusy.value = true;
      try {
        await toggleAppEnablement(props.item);
      } finally {
        toggleBusy.value = false;
      }
    };

    /** 空间启用情况（install-and-enable §一：同一份代码按空间实例化，启用各自生效）：
     *  只列**已启用**的空间（走查修正：系统层详情不列未启用的）——
     *  个人空间 + 我加入的组织（org-membership 缓存），逐空间给 最近使用 / 当前空间 标记 */
    const enabledSpaceRows = computed(() => {
      const spaces: Array<{ space: EnablementSpace; key: string; name: string; recentKey: string }> = [
        { space: { type: 'personal' }, key: 'personal', name: personalSpaceName.value, recentKey: 'personal' },
        ...organizations.value.map((org) => ({
          space: { type: 'org', orgId: org.orgId } as EnablementSpace,
          key: `org:${org.orgId}`,
          name: org.name,
          recentKey: org.orgId
        }))
      ];
      const currentKey =
        currentSpace.value.type === 'org' ? `org:${currentSpace.value.orgId}` : 'personal';
      return spaces
        .filter(({ space }) => isAppEnableableInSpace(space, props.item) && isAppEnabledInSpace(space, props.item))
        .map(({ key, name, recentKey }) => ({
          key,
          name,
          isCurrent: key === currentKey,
          usedRecently: recentIdsOf(recentKey).includes(props.item.id)
        }));
    });

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
      developerText,
      sourceRepoUrl,
      hasSignature,
      enabledHere,
      canToggleHere,
      toggleBusy,
      onToggleHere,
      enabledSpaceRows,
      openExternal,
      copyText,
      isMobileLayout,
      emit
    };
  }
});
</script>
