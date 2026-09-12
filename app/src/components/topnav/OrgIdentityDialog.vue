<!-- 组织空间「身份设置」承载对话框（problem D2，走查修正后）：
     域内身份资料随空间不同而不同（域内昵称/头像/签名等 + 成员权限黑名单），
     由顶栏「当前身份」菜单在组织空间下打开；个人空间的身份设置走「我的」（MinePage）。
     「我的」改为系统根级页面后（不随空间切换），组织身份/成员权限不再混入 MinePage，
     统一收进本对话框。弹法同 L12 壳层模态（顶级对话框＋遮罩）。 -->
<template>
  <el-dialog
    v-model="visible"
    :title="`身份设置${orgName ? ` · ${orgName}` : ''}`"
    width="min(720px, 92vw)"
    class="topnav-org-identity"
  >
    <el-tabs v-model="tab">
      <el-tab-pane label="组织身份" name="identity">
        <div class="topnav-org-identity-body">
          <OrgIdentityModule :key="`identity-${orgId}`" />
        </div>
      </el-tab-pane>
      <el-tab-pane label="成员权限" name="permission">
        <div class="topnav-org-identity-body">
          <PermissionModule :key="`permission-${orgId}`" mode="org" />
        </div>
      </el-tab-pane>
    </el-tabs>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { currentSpaceOrgId } from '../../stores/current-space';
import { findOrg } from '../../stores/org-membership';
import OrgIdentityModule from '../mine/OrgIdentityModule.vue';
import PermissionModule from '../mine/PermissionModule.vue';

export default defineComponent({
  name: 'OrgIdentityDialog',
  components: { OrgIdentityModule, PermissionModule },
  props: {
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const visible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });
    const tab = ref('identity');

    // 内容跟随当前空间组织；打开期间切走组织空间则自动关闭
    const orgId = currentSpaceOrgId;
    const orgName = computed(() => findOrg(orgId.value)?.name ?? '');
    watch(orgId, (id) => {
      if (!id && props.modelValue) {
        emit('update:modelValue', false);
      }
    });

    return { visible, tab, orgId, orgName };
  }
});
</script>

<style scoped>
/* 模块自带列表栏＋详情栏（MinePage 第三、四栏同款）；对话框内限高、内部滚动 */
.topnav-org-identity-body {
  display: flex;
  height: min(560px, 66vh);
  min-height: 0;
}

.topnav-org-identity-body > * {
  flex: 1;
  min-width: 0;
  min-height: 0;
}
</style>
