<!-- 「创建 / 加入组织」顶级对话框宿主（problem L10）：挂在 App 根，经 org-membership-dialog
     store 打开（rail「空间」＋菜单等入口调用 openMembershipDialog），弹法同全局搜索——
     顶级 z-index、半透明遮罩锁定背景、点遮罩 / Esc 关闭、创建 / 加入互斥单实例。
     修复原缺陷：对话框原挂在 RailSpaceList 内、经 v-for 模板 ref 转发打开（ref 拿到实例数组，
     转发静默失效 → 打不开），且嵌套在 rail 里非顶级；现提升到外壳根。 -->
<template>
  <CreateOrgDialog v-model="createVisible" :creating="busy" @submit="createOrganization" />
  <JoinOrgDialog v-model="joinVisible" :joining="busy" @submit="acceptInvite" />
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { refreshOrganizations } from '../../stores/org-membership';
import { switchToOrg } from '../../stores/current-space';
import { setOrgAvatar } from '../../stores/org-avatars';
import { closeMembershipDialog, membershipDialogMode } from '../../stores/org-membership-dialog';
import CreateOrgDialog from './CreateOrgDialog.vue';
import JoinOrgDialog from './JoinOrgDialog.vue';
import type { CreateForm } from './types';

export default defineComponent({
  name: 'MembershipDialogs',
  components: { CreateOrgDialog, JoinOrgDialog },
  setup() {
    const busy = ref(false);
    const visibleOf = (mode: 'create' | 'join') =>
      computed({
        get: () => membershipDialogMode.value === mode,
        set: (value: boolean) => {
          if (!value && membershipDialogMode.value === mode) closeMembershipDialog();
        }
      });
    const createVisible = visibleOf('create');
    const joinVisible = visibleOf('join');

    const createOrganization = async (form: CreateForm) => {
      if (busy.value) return;
      busy.value = true;
      try {
        const org = await window.electronAPI.organization.create(form);
        if (org.avatar) setOrgAvatar(org.orgId, org.avatar);
        await refreshOrganizations();
        closeMembershipDialog();
        switchToOrg(org.orgId);
        ElMessage.success(`已创建「${org.name}」`);
      } catch (error) {
        ElMessage.error(error instanceof Error ? error.message : '创建失败');
      } finally { busy.value = false; }
    };

    const acceptInvite = async (code: string) => {
      if (busy.value || !code.trim()) return;
      busy.value = true;
      try {
        const joined = await window.electronAPI.organization.acceptInvite(code.trim());
        await refreshOrganizations();
        closeMembershipDialog();
        switchToOrg(joined.orgId);
        ElMessage.success(`已加入「${joined.orgName}」`);
      } catch (error) {
        ElMessage.error(error instanceof Error ? error.message : '加入失败');
      } finally { busy.value = false; }
    };

    return { createVisible, joinVisible, busy, createOrganization, acceptInvite };
  }
});
</script>
