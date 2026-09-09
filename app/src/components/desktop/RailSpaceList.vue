<!-- rail「空间」二级列表（PC）：空间项下方列出 个人空间 + 已加入组织，点击切换当前空间。
     出处：用户评审决策（顶导航左上角的空间切换收进「空间」，二级列表放 rail「空间」二字下面）。
     数据源复用：org-membership（organizations）、current-space（切换）、头像组件。
     仅在 rail 展开（宽栏）且当前 tab=空间时展示，窄栏态不展开子列表（保持图标简洁）。 -->
<template>
  <div class="rail-space-list">
    <el-input v-if="organizations.length > 3" v-model="query" size="small" placeholder="搜索空间" :prefix-icon="Search" clearable />
    <button
      type="button"
      class="rail-space-item"
      :class="{ active: isPersonal }"
      title="个人空间"
      @click="select({ type: 'personal' })"
    >
      <UserAvatar :root-id="personalSource.seed" :nickname="personalSource.name" :avatar="personalSource.image" :size="22" />
      <span class="rail-space-name">个人空间</span>
    </button>

    <button
      v-for="org in filteredOrganizations"
      :key="org.orgId"
      type="button"
      class="rail-space-item"
      :class="{ active: !isPersonal && currentOrgId === org.orgId }"
      :title="org.name"
      @click="select({ type: 'org', orgId: org.orgId })"
    >
      <OrgAvatar :org-id="org.orgId" :name="org.name" :size="22" />
      <span class="rail-space-name">{{ org.name }}</span>
    </button>

    <!-- 创建/加入组织入口 -->
    <!-- 创建/加入入口移出本列表：由「空间」行右侧「＋」触发本组件对话框（App.vue 转发） -->
    <CreateOrgDialog v-model="createVisible" :creating="busy" @submit="createOrganization" />
    <JoinOrgDialog v-model="joinVisible" :joining="busy" @submit="acceptInvite" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { Search } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { organizations, refreshOrganizations } from '../../stores/org-membership';
import { currentSpace, currentSpaceOrgId, switchToOrg, switchToPersonal, type CurrentSpace } from '../../stores/current-space';
import { personalAvatarSource } from '../../stores/avatar-sources';
import UserAvatar from '../UserAvatar.vue';
import OrgAvatar from '../OrgAvatar.vue';
import CreateOrgDialog from '../org/CreateOrgDialog.vue';
import JoinOrgDialog from '../org/JoinOrgDialog.vue';
import { setOrgAvatar } from '../../stores/org-avatars';
import type { CreateForm } from '../org/types';

export default defineComponent({
  name: 'RailSpaceList',
  components: { UserAvatar, OrgAvatar, CreateOrgDialog, JoinOrgDialog },
  setup(_, { expose }) {
    const query = ref('');
    const filteredOrganizations = computed(() => organizations.value.filter((org) => org.name.toLocaleLowerCase().includes(query.value.trim().toLocaleLowerCase())));
    const createVisible = ref(false);
    const joinVisible = ref(false);
    const busy = ref(false);
    /** 供 App.vue「空间」行右侧「＋」调用：打开创建 / 加入对话框 */
    const openMembership = (mode: 'create' | 'join') => {
      if (mode === 'create') createVisible.value = true;
      else joinVisible.value = true;
    };
    expose({ openMembership });
    const createOrganization = async (form: CreateForm) => {
      if (busy.value) return;
      busy.value = true;
      try {
        const org = await window.electronAPI.organization.create(form);
        if (org.avatar) setOrgAvatar(org.orgId, org.avatar);
        await refreshOrganizations();
        createVisible.value = false;
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
        joinVisible.value = false;
        switchToOrg(joined.orgId);
        ElMessage.success(`已加入「${joined.orgName}」`);
      } catch (error) {
        ElMessage.error(error instanceof Error ? error.message : '加入失败');
      } finally { busy.value = false; }
    };
    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    const currentOrgId = currentSpaceOrgId;
    const personalSource = computed(() => personalAvatarSource());

    onMounted(() => {
      void refreshOrganizations().catch(() => {});
    });

    const select = (space: CurrentSpace) => {
      if (space.type === 'personal') {
        switchToPersonal();
      } else {
        switchToOrg(space.orgId);
      }
      // 二级菜单点击 = 切换空间并进入该桌面（用户评审：点击要有反应）
      window.dispatchEvent(new CustomEvent('spark:switch-tab', { detail: 'space' }));
    };

    return { organizations, filteredOrganizations, query, Search, createVisible, joinVisible, busy, openMembership, createOrganization, acceptInvite, isPersonal, currentOrgId, personalSource, select };
  }
});
</script>

<style scoped>
.rail-space-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 2px 0 4px;
  padding-left: 8px;
}

.rail-space-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: 0;
  border-radius: var(--spark-radius-m);
  background: transparent;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
  color: var(--spark-rail-text);
}

.rail-space-item:hover {
  background: var(--spark-rail-item-hover);
}

.rail-space-item.active {
  background: var(--spark-rail-item-active);
  color: var(--spark-primary);
}

.rail-space-name {
  font-size: var(--spark-font-size-placeholder);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
  min-width: 0;
}

/* 列表底部无独立创建/加入行（入口在「空间」行右侧「＋」），原 .rail-space-add 样式删除 */
</style>
