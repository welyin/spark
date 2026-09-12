<!-- 应用属性面板（桌面窗口内容，走查修正：属性开成桌面窗口而非对话框）：
     由 WindowFrame 承载（appId='spark:app-properties'），目标应用经 viewBootstrap.cardData.appId 传入。
     系统层只读信息（版本/简介/权限/签名来源/已启用空间）＋唯一操作＝当前空间的启用/停用。
     装卸不在这里（系统层应用管理）；组织空间非管理员只读（过渡期管理员操作仅本机生效）。 -->
<template>
  <div v-if="item" class="app-properties-panel">
    <section class="app-detail-hero">
      <AppIcon class="app-detail-icon" :item="item" />
      <div class="app-detail-hero-info">
        <h1>{{ item.name }}</h1>
        <div class="app-detail-meta-list">
          <div class="app-detail-meta-row">
            <span class="app-detail-meta-label">版本</span>
            <span class="app-detail-meta-value">{{ item.installedVersion ?? item.version }}</span>
          </div>
          <div class="app-detail-meta-row">
            <span class="app-detail-meta-label">开发者</span>
            <span class="app-detail-meta-value">{{ developerText }}</span>
          </div>
        </div>
      </div>
    </section>

    <!-- 唯一操作：当前空间的启用/停用（空间层动作） -->
    <section class="app-detail-section">
      <div class="app-detail-section-title">
        <h2>在当前空间（{{ spaceName }}）</h2>
      </div>
      <div class="app-properties-enable-row">
        <el-switch
          :model-value="enabledHere"
          :disabled="!canToggle || busy"
          @change="onToggle"
        />
        <span class="app-properties-enable-text">{{ enabledHere ? '已启用' : '未启用' }}</span>
      </div>
      <p v-if="isOrgSpace" class="app-detail-space-note">
        {{ canToggle
          ? '过渡期：启停仅本机生效，成员间同步待内核「组织启用清单」落地；规则类应用的正式启用需走组织事务决议。'
          : '启用/停用需走组织流程（过渡期由管理员在本机操作）。' }}
      </p>
    </section>

    <!-- 已启用的空间（只读；未启用不列） -->
    <section class="app-detail-section">
      <div class="app-detail-section-title">
        <h2>空间启用情况</h2>
      </div>
      <ul v-if="enabledSpaces.length > 0" class="app-detail-spaces">
        <li v-for="row in enabledSpaces" :key="row.key" class="app-detail-space-row">
          <span class="app-detail-space-name">{{ row.name }}</span>
          <el-tag size="small" effect="plain" type="success">已启用</el-tag>
        </li>
      </ul>
      <p v-else class="app-detail-muted">未在任何空间启用</p>
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
          {{ hasSignature ? '已提供签名' : '未提供签名' }}
        </span>
      </div>
      <p class="app-detail-source-note" :class="hasSignature ? '' : 'app-detail-source-note--warn'">
        {{ hasSignature ? `签名地址：${item.package.signatureUrl}` : '该应用未提供签名，来源未经核验。' }}
      </p>
    </section>
  </div>
  <el-empty v-else description="读取应用信息失败" />
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import type { PluginMarketItemDto } from '../../api/types';
import { permissionLabel } from '../apps/apps-store';
import AppIcon from '../apps/AppIcon.vue';
import { currentSpace } from '../../stores/current-space';
import { findOrg, isAdmin, organizations } from '../../stores/org-membership';
import { personalSpaceName } from '../../stores/personal-space';
import {
  isAppEnabledInSpace,
  setAppEnabledInSpace,
  type EnablementSpace
} from '../../stores/app-enablement';

export default defineComponent({
  name: 'AppPropertiesPanel',
  components: { AppIcon },
  props: {
    /** 目标应用 id（窗口 viewBootstrap.cardData.appId 传入） */
    appId: { type: String, required: true }
  },
  setup(props) {
    const item = ref<PluginMarketItemDto | null>(null);
    const busy = ref(false);

    // 挂载 / 目标切换时按 appId 拉市场条目（含权限/签名等完整信息）
    watch(
      () => props.appId,
      async () => {
        item.value = null;
        try {
          const list = await window.electronAPI.pluginMarket.list();
          item.value = list.find((entry) => entry.id === props.appId) ?? null;
        } catch {
          item.value = null;
        }
      },
      { immediate: true }
    );

    const isOrgSpace = computed(() => currentSpace.value.type === 'org');
    const spaceName = computed(() =>
      currentSpace.value.type === 'org'
        ? (findOrg(currentSpace.value.orgId)?.name ?? '组织空间')
        : personalSpaceName.value
    );
    const space = computed<EnablementSpace>(() =>
      currentSpace.value.type === 'org'
        ? { type: 'org', orgId: currentSpace.value.orgId }
        : { type: 'personal' }
    );
    const canToggle = computed(() =>
      currentSpace.value.type === 'org' ? isAdmin(currentSpace.value.orgId) : true
    );
    const enabledHere = computed(() =>
      item.value ? isAppEnabledInSpace(space.value, item.value) : false
    );

    /** 开发者展示：仓库锚定插件取 owner 段；其余为域名持有者（与 AppDetailPanel 同口径） */
    const developerText = computed(() => {
      const segments = props.appId.split('/');
      return segments.length >= 3 ? segments[1] : (item.value?.domain ?? props.appId);
    });

    const hasSignature = computed(() => Boolean(item.value?.package.signatureUrl));

    /** 已启用的空间（只读）：个人空间 + 我加入的组织 */
    const enabledSpaces = computed(() => {
      const current = item.value;
      if (!current) {
        return [];
      }
      const rows = [
        { key: 'personal', name: personalSpaceName.value, space: { type: 'personal' } as EnablementSpace },
        ...organizations.value.map((org) => ({
          key: `org:${org.orgId}`,
          name: org.name,
          space: { type: 'org', orgId: org.orgId } as EnablementSpace
        }))
      ];
      return rows.filter((row) => isAppEnabledInSpace(row.space, current));
    });

    /** 启用/停用（当前空间）：写 per-space 事实源；个人空间回写内核全局开关（失败回滚）。
        与 AppsPage toggleEnabled 同口径（过渡期本机生效，诚实标注） */
    const onToggle = async () => {
      const current = item.value;
      if (!current || !canToggle.value) {
        return;
      }
      const next = !enabledHere.value;
      setAppEnabledInSpace(space.value, current.id, next);
      if (isOrgSpace.value) {
        ElMessage.success(next ? '已在本机启用（仅本机生效，组织内同步待内核）' : '已在本机停用（仅本机生效）');
        return;
      }
      busy.value = true;
      try {
        await window.electronAPI.pluginMarket.setEnabled(current.id, next);
      } catch (error) {
        setAppEnabledInSpace(space.value, current.id, !next);
        ElMessage.error(`应用启停失败：${error}`);
      } finally {
        busy.value = false;
      }
    };

    return {
      item,
      busy,
      isOrgSpace,
      spaceName,
      canToggle,
      enabledHere,
      developerText,
      hasSignature,
      enabledSpaces,
      onToggle,
      permissionLabel
    };
  }
});
</script>

<style scoped>
.app-properties-panel {
  height: 100%;
  overflow-y: auto;
  padding: 16px 20px 24px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.app-properties-enable-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
</style>
