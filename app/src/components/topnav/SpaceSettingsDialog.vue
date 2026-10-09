<!-- 顶栏「当前空间 → 空间设置」承载对话框（problem D1）：
     复用组织空间的 OrgSettingsPanel（原打开路径为 SettingsPage「空间」菜单，面板本身
     只服务当前空间组织），此处以顶级对话框＋遮罩弹出（规格同 L12 壳层模态：
     Element 弹层顶级 z-index、遮罩锁定背景、点遮罩 / Esc 关闭）。
     仅组织空间可打开（个人空间菜单项置灰，不会走到这里）。 -->
<template>
  <el-dialog
    v-model="visible"
    :title="`空间设置${orgName ? ` · ${orgName}` : ''}`"
    width="min(960px, 92vw)"
    class="topnav-space-settings"
  >
    <div class="topnav-space-settings-body">
      <!-- 组织管理（A42 灰度，与消息/市场同口径）：旧内置 UI ⇄ 默认内置插件版
           （spark-org-admin，sdk.org 数据面）；加载失败「关闭」回退 legacy -->
      <BuiltinAppHost v-if="orgId" :key="orgId" tab-id="org" :space="pluginSpace" @fallback="onOrgFallback">
        <template #legacy>
          <OrgSettingsPanel />
        </template>
      </BuiltinAppHost>
    </div>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, watch } from 'vue';
import { currentSpace, currentSpaceOrgId } from '../../stores/current-space';
import { findOrg } from '../../stores/org-membership';
import { setBuiltinImpl } from '../../stores/builtin-apps';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import BuiltinAppHost from '../plugin/BuiltinAppHost.vue';
import OrgSettingsPanel from '../org/OrgSettingsPanel.vue';

export default defineComponent({
  name: 'SpaceSettingsDialog',
  components: { BuiltinAppHost, OrgSettingsPanel },
  props: {
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const visible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    // 面板跟随当前空间组织；打开期间切走组织空间则自动关闭
    const orgId = currentSpaceOrgId;
    const orgName = computed(() => findOrg(orgId.value)?.name ?? '');
    watch(orgId, (id) => {
      if (!id && props.modelValue) {
        emit('update:modelValue', false);
      }
    });

    /** 插件运行 space 上下文（透传 BuiltinAppHost；本对话框仅组织空间可打开） */
    const pluginSpace = computed<PluginSpaceContext>(() => ({
      type: currentSpace.value.type,
      id: currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
    }));

    // 插件版加载失败/被关闭：回退旧内置 UI（与 messages/apps 同口径灰度兜底）
    const onOrgFallback = () => setBuiltinImpl('org', 'legacy');

    return { visible, orgId, orgName, pluginSpace, onOrgFallback };
  }
});
</script>

<style scoped>
/* 同 ShellModals 的壳层页面规格：固定限高、面板内部各栏自行滚动、贴边去 body 内边距 */
.topnav-space-settings-body {
  display: flex;
  height: min(720px, 78vh);
  min-height: 0;
}

.topnav-space-settings-body > * {
  flex: 1;
  min-width: 0;
  min-height: 0;
}

.topnav-space-settings :deep(.el-dialog__body) {
  padding: 0;
}
</style>
