<!-- 顶部空间切换器：当前空间 logo+名称，点击展开下拉（个人空间 / 组织列表 / 加入组织 / 创建组织），网络状态点由右侧 NetworkStatusBar 提供 -->
<template>
  <div class="space-switcher">
    <el-popover ref="popoverRef" placement="bottom" :width="264" trigger="click" @show="refreshOrganizations">
      <template #reference>
        <button class="space-switcher-trigger" :title="`当前空间：${currentSpaceName}`">
          <UserAvatar
            v-if="isPersonal"
            :root-id="personalSource.seed"
            :nickname="personalSource.name"
            :avatar="personalSource.image"
            :size="26"
          />
          <OrgAvatar v-else :org-id="currentOrgId" :name="currentSpaceName" :size="26" />
          <span class="space-switcher-name">{{ currentSpaceName }}</span>
          <el-icon :size="12" class="space-switcher-arrow"><ArrowDown /></el-icon>
        </button>
      </template>

      <div class="space-menu">
        <button class="space-menu-item" :class="{ active: isPersonal }" @click="selectPersonal">
          <UserAvatar :root-id="personalSource.seed" :nickname="personalSource.name" :avatar="personalSource.image" :size="26" />
          <span class="space-menu-name">个人空间</span>
          <el-icon v-if="isPersonal" class="space-menu-check"><Check /></el-icon>
        </button>

        <template v-if="organizations.length > 0">
          <div class="space-menu-divider" />
          <div class="space-menu-orgs">
            <button
              v-for="org in organizations"
              :key="org.orgId"
              class="space-menu-item"
              :class="{ active: !isPersonal && currentOrgId === org.orgId }"
              @click="selectOrg(org.orgId)"
            >
              <OrgAvatar :org-id="org.orgId" :name="org.name" :size="26" />
              <span class="space-menu-name">{{ org.name }}</span>
              <el-icon v-if="!isPersonal && currentOrgId === org.orgId" class="space-menu-check"><Check /></el-icon>
            </button>
          </div>
        </template>

        <div class="space-menu-divider" />
        <button class="space-menu-item space-menu-action" @click="openJoinDialog">
          <el-icon :size="16"><Connection /></el-icon>
          <span class="space-menu-name">加入组织</span>
        </button>
        <button class="space-menu-item space-menu-action" @click="openCreateDialog">
          <el-icon :size="16"><Plus /></el-icon>
          <span class="space-menu-name">创建组织</span>
        </button>
      </div>
    </el-popover>

    <CreateOrgDialog
      ref="createDialogRef"
      v-model="createDialogVisible"
      :creating="creating"
      @submit="createOrganization"
    />

    <JoinOrgDialog v-model="joinDialogVisible" :joining="joining" @submit="acceptInvite" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { ArrowDown, Check, Connection, Plus } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId, switchToOrg, switchToPersonal } from '../stores/current-space';
import { organizations, refreshOrganizations as refreshOrgMembership } from '../stores/org-membership';
import { setOrgAvatar } from '../stores/org-avatars';
import { personalAvatarSource } from '../stores/avatar-sources';
import UserAvatar from './UserAvatar.vue';
import OrgAvatar from './OrgAvatar.vue';
import CreateOrgDialog from './org/CreateOrgDialog.vue';
import JoinOrgDialog from './org/JoinOrgDialog.vue';
import type { CreateForm } from './org/types';

export default defineComponent({
  name: 'SpaceSwitcher',
  components: {
    UserAvatar,
    OrgAvatar,
    CreateOrgDialog,
    JoinOrgDialog,
    ArrowDown,
    Check,
    Connection,
    Plus
  },
  setup() {
    // 非受控模式（trigger=click 自带点击外部关闭）；菜单项点击后通过 ref 手动收起
    const popoverRef = ref<{ hide: () => void } | null>(null);
    const createDialogVisible = ref(false);
    const creating = ref(false);
    const createDialogRef = ref<{ resetAfterCreate: () => void } | null>(null);
    const joinDialogVisible = ref(false);
    const joining = ref(false);

    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    const currentOrgId = currentSpaceOrgId;

    const currentSpaceName = computed(() => {
      if (isPersonal.value) {
        return '个人空间';
      }
      return organizations.value.find((org) => org.orgId === currentOrgId.value)?.name ?? '组织空间';
    });

    const refreshOrganizations = async () => {
      try {
        await refreshOrgMembership();
      } catch {
        // 列表读取失败保留旧数据，下拉仍可切换个人空间
      }
    };

    // 点击任意菜单项后手动收起下拉（popover 内容区点击不会自动关闭）
    const closeMenu = () => {
      popoverRef.value?.hide();
    };

    const selectPersonal = () => {
      closeMenu();
      switchToPersonal();
    };

    const selectOrg = (orgId: string) => {
      closeMenu();
      switchToOrg(orgId);
    };

    const openCreateDialog = () => {
      closeMenu();
      createDialogVisible.value = true;
    };

    const openJoinDialog = () => {
      closeMenu();
      joinDialogVisible.value = true;
    };

    const acceptInvite = async (code: string) => {
      if (!code.trim()) {
        ElMessage.warning('请输入邀请码');
        return;
      }

      joining.value = true;
      try {
        const joined = await window.electronAPI.organization.acceptInvite(code.trim());
        ElMessage.success(`已加入组织：${joined.orgName}`);
        joinDialogVisible.value = false;
        await refreshOrganizations();
        // 与创建成功一致：加入后自动切换到该组织空间
        switchToOrg(joined.orgId);
      } catch (error) {
        ElMessage.error(`加入组织失败：${error}`);
      } finally {
        joining.value = false;
      }
    };

    const createOrganization = async (form: CreateForm) => {
      if (!form.name.trim()) {
        ElMessage.warning('请输入组织名称');
        return;
      }

      creating.value = true;
      try {
        const created = await window.electronAPI.organization.create({
          name: form.name,
          description: form.description
        });
        if (form.avatar) {
          setOrgAvatar(created.orgId, form.avatar);
        }
        ElMessage.success(`组织已创建：${created.name}`);
        createDialogRef.value?.resetAfterCreate();
        createDialogVisible.value = false;
        await refreshOrganizations();
        // 设计 §7.2：创建成功后自动切换到新组织空间
        switchToOrg(created.orgId);
      } catch (error) {
        ElMessage.error(`创建组织失败：${error}`);
      } finally {
        creating.value = false;
      }
    };

    onMounted(() => {
      void refreshOrganizations();
    });

    return {
      organizations,
      isPersonal,
      currentOrgId,
      currentSpaceName,
      popoverRef,
      createDialogVisible,
      creating,
      createDialogRef,
      joinDialogVisible,
      joining,
      refreshOrganizations,
      selectPersonal,
      selectOrg,
      openCreateDialog,
      openJoinDialog,
      acceptInvite,
      createOrganization,
      // 个人空间头像统一从 avatar-sources 取数（种子=rootId），不再经 props 透传
      personalSource: computed(() => personalAvatarSource())
    };
  }
});
</script>

<style scoped>
.space-switcher {
  display: inline-flex;
}

.space-switcher-trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 320px;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  font-size: var(--spark-font-size-base);
  font-weight: 600;
  color: var(--spark-text-1);
  padding: 4px 10px;
  border-radius: var(--spark-radius-l);
  -webkit-app-region: no-drag;
}

.space-switcher-trigger:hover {
  background: var(--spark-bg-hover);
}

.space-switcher-name {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.space-switcher-arrow {
  color: var(--spark-text-3);
  flex-shrink: 0;
}

.space-menu {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: -12px;
  padding: 6px;
}

.space-menu-orgs {
  max-height: 260px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.space-menu-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  font-size: var(--spark-font-size-placeholder);
  color: var(--spark-text-1);
  padding: 7px 10px;
  border-radius: var(--spark-radius-m);
  text-align: left;
}

.space-menu-item:hover {
  background: var(--spark-bg-hover);
}

.space-menu-item.active {
  background: var(--spark-primary-light);
}

.space-menu-item.active .space-menu-name {
  color: var(--spark-primary);
}

.space-menu-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.space-menu-check {
  color: var(--spark-primary);
  flex-shrink: 0;
}

.space-menu-divider {
  height: 1px;
  margin: 4px 2px;
  background: var(--spark-border-light);
}

.space-menu-action {
  color: var(--spark-text-2);
}
</style>
