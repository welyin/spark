<!-- 空间域列表页（手机端 space tab 的 root，一级）：个人空间置顶 + 已加入组织列表。
     出处 shell-mobile §3.1：第一级「我的手机桌面列表」，个人空间置顶、各域带待办角标、
     右上角 ＋ 创建/加入组织；点某域 push 进该域手机桌面（两级结构第二级）。
     数据源复用：org-membership（organizations）、current-space（切换）、
     space-notifications（域角标）、CreateOrgDialog/JoinOrgDialog（创建/加入）。 -->
<template>
  <div class="space-list-page">
    <!-- 顶部标题栏 + ＋（创建/加入） -->
    <header class="space-list-header">
      <h1 class="space-list-title">空间</h1>
      <button type="button" class="space-list-add" title="创建 / 加入组织" @click="joinCreateVisible = true">
        <el-icon :size="20"><Plus /></el-icon>
      </button>
    </header>

    <div class="space-list-body">
      <!-- 个人空间（固定置顶） -->
      <button
        type="button"
        class="space-item"
        @click="openSpace({ type: 'personal' })"
      >
        <span class="space-item-avatar" :class="{ 'has-badge': personalBadge > 0 }">
          <UserAvatar :root-id="personalSource.seed" :nickname="personalSource.name" :avatar="personalSource.image" :size="44" />
          <el-badge v-if="personalBadge > 0" :value="personalBadge" :max="99" class="space-item-badge" />
        </span>
        <span class="space-item-main">
          <span class="space-item-name">个人空间</span>
          <span class="space-item-sub">我的私人桌面</span>
        </span>
        <el-icon class="space-item-arrow"><ArrowRight /></el-icon>
      </button>

      <!-- 组织空间列表 -->
      <button
        v-for="org in organizations"
        :key="org.orgId"
        type="button"
        class="space-item"
        @click="openSpace({ type: 'org', orgId: org.orgId })"
      >
        <span class="space-item-avatar" :class="{ 'has-badge': orgBadge(org.orgId) > 0 }">
          <OrgAvatar :org-id="org.orgId" :name="org.name" :size="44" />
          <el-badge v-if="orgBadge(org.orgId) > 0" :value="orgBadge(org.orgId)" :max="99" class="space-item-badge" />
        </span>
        <span class="space-item-main">
          <span class="space-item-name">{{ org.name }}</span>
          <span class="space-item-sub">组织空间</span>
        </span>
        <el-icon class="space-item-arrow"><ArrowRight /></el-icon>
      </button>

      <!-- 空态：无组织时引导创建/加入 -->
      <div v-if="organizations.length === 0" class="space-list-empty">
        <el-empty :image-size="100" description="还没有加入任何组织">
          <el-button type="primary" @click="joinCreateVisible = true">创建或加入组织</el-button>
        </el-empty>
      </div>
    </div>

    <!-- 创建/加入 上滑菜单 -->
    <Teleport to="body">
      <Transition name="mobile-sheet">
        <div v-if="joinCreateVisible" class="space-sheet-root" @click="joinCreateVisible = false">
          <div class="space-sheet" @click.stop>
            <button type="button" class="space-sheet-item" @click="openCreate">
              <el-icon :size="18"><OfficeBuilding /></el-icon>
              <div class="space-sheet-text">
                <b>创建组织</b>
                <span>创建一个新的组织空间</span>
              </div>
            </button>
            <button type="button" class="space-sheet-item" @click="openJoin">
              <el-icon :size="18"><Connection /></el-icon>
              <div class="space-sheet-text">
                <b>加入组织</b>
                <span>通过邀请码加入已有组织</span>
              </div>
            </button>
            <button type="button" class="space-sheet-cancel" @click="joinCreateVisible = false">取消</button>
          </div>
        </div>
      </Transition>

      <CreateOrgDialog v-model="createVisible" :creating="creating" @submit="createOrganization" />
      <JoinOrgDialog v-model="joinVisible" :joining="joining" @submit="acceptInvite" />
    </Teleport>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { ArrowRight, Connection, OfficeBuilding, Plus } from '@element-plus/icons-vue';
import { organizations, refreshOrganizations } from '../stores/org-membership';
import { switchToOrg, switchToPersonal, type CurrentSpace } from '../stores/current-space';
import { unreadCountOf } from '../stores/messages';
import { spaceKeyOf } from '../mock/contacts';
import { personalAvatarSource } from '../stores/avatar-sources';
import { setOrgAvatar } from '../stores/org-avatars';
import { pushPage } from '../stores/mobile-nav';
import UserAvatar from './UserAvatar.vue';
import OrgAvatar from './OrgAvatar.vue';
import CreateOrgDialog from './org/CreateOrgDialog.vue';
import JoinOrgDialog from './org/JoinOrgDialog.vue';
import type { CreateForm } from './org/types';

const TAB = 'space';

export default defineComponent({
  name: 'SpaceListPage',
  components: {
    UserAvatar,
    OrgAvatar,
    CreateOrgDialog,
    JoinOrgDialog,
    Plus,
    ArrowRight,
    Connection,
    OfficeBuilding
  },
  setup() {
    const joinCreateVisible = ref(false);
    const createVisible = ref(false);
    const creating = ref(false);
    const joinVisible = ref(false);
    const joining = ref(false);

    const personalSource = computed(() => personalAvatarSource());

    // 域待办角标（第一版近似：未读消息数；「待我处理事务数」待 affairs 数据源接入，见 ui-architecture §七）
    const personalBadge = computed(() => unreadCountOf(spaceKeyOf({ type: 'personal' })));
    const orgBadge = (orgId: string) => unreadCountOf(spaceKeyOf({ type: 'org', orgId }));

    onMounted(() => {
      void refreshOrganizations().catch(() => {});
    });

    /** 进入某张桌面：切域 + push 桌面栈帧（两级结构第二级，1.2 承接） */
    const openSpace = (space: CurrentSpace) => {
      if (space.type === 'personal') {
        switchToPersonal();
        pushPage(TAB, 'desktop', { id: 'personal' });
      } else {
        switchToOrg(space.orgId);
        pushPage(TAB, 'desktop', { id: space.orgId });
      }
    };

    const openCreate = () => {
      joinCreateVisible.value = false;
      createVisible.value = true;
    };
    const openJoin = () => {
      joinCreateVisible.value = false;
      joinVisible.value = true;
    };

    const createOrganization = async (form: CreateForm) => {
      creating.value = true;
      try {
        const org = await window.electronAPI.organization.create(form);
        if (org.avatar) {
          setOrgAvatar(org.orgId, org.avatar);
        }
        await refreshOrganizations().catch(() => {});
        createVisible.value = false;
        ElMessage.success(`已创建「${org.name}」`);
        openSpace({ type: 'org', orgId: org.orgId });
      } catch (err) {
        ElMessage.error(err instanceof Error ? err.message : '创建失败');
      } finally {
        creating.value = false;
      }
    };

    const acceptInvite = async (inviteCode: string) => {
      if (!inviteCode.trim()) {
        ElMessage.warning('请输入邀请码');
        return;
      }
      joining.value = true;
      try {
        const joined = await window.electronAPI.organization.acceptInvite(inviteCode.trim());
        await refreshOrganizations().catch(() => {});
        joinVisible.value = false;
        ElMessage.success(`已加入「${joined.orgName}」`);
        openSpace({ type: 'org', orgId: joined.orgId });
      } catch (err) {
        ElMessage.error(err instanceof Error ? err.message : '加入失败');
      } finally {
        joining.value = false;
      }
    };

    return {
      organizations,
      personalSource,
      personalBadge,
      orgBadge,
      joinCreateVisible,
      createVisible,
      creating,
      joinVisible,
      joining,
      openSpace,
      openCreate,
      openJoin,
      createOrganization,
      acceptInvite
    };
  }
});
</script>

<style scoped>
.space-list-page {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page);
}

.space-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px var(--spark-padding-page);
  background: var(--spark-bg-card);
  border-bottom: 1px solid var(--spark-border-light);
}

.space-list-title {
  margin: 0;
  font-size: var(--spark-font-size-title);
  font-weight: 600;
  color: var(--spark-text-1);
}

.space-list-add {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  border: 0;
  border-radius: var(--spark-radius-m);
  background: transparent;
  color: var(--spark-text-1);
  cursor: pointer;
}

.space-list-add:hover {
  background: var(--spark-bg-hover);
}

.space-list-body {
  flex: 1;
  overflow-y: auto;
  padding: 8px 0;
}

.space-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 10px var(--spark-padding-page);
  border: 0;
  background: transparent;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
}

.space-item:hover {
  background: var(--spark-bg-hover);
}

.space-item-avatar {
  position: relative;
  flex-shrink: 0;
  line-height: 0;
}

.space-item-badge {
  position: absolute;
  top: -4px;
  right: -4px;
}

.space-item-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.space-item-name {
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.space-item-sub {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.space-item-arrow {
  flex-shrink: 0;
  color: var(--spark-text-3);
}

.space-list-empty {
  padding: 40px 0;
}

/* 上滑菜单（与 MobileSpaceDrawer 同套样式语义） */
.space-sheet-root {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
  background: rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: flex-end;
}

.space-sheet {
  width: 100%;
  background: var(--spark-bg-card);
  border-radius: var(--spark-radius-xl) var(--spark-radius-xl) 0 0;
  padding: 8px 0 calc(8px + var(--spark-safe-bottom, env(safe-area-inset-bottom, 0px)));
}

.space-sheet-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 14px var(--spark-padding-page);
  border: 0;
  background: transparent;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
}

.space-sheet-item:hover {
  background: var(--spark-bg-hover);
}

.space-sheet-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.space-sheet-text b {
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
  font-weight: 600;
}

.space-sheet-text span {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.space-sheet-cancel {
  width: 100%;
  padding: 14px 0;
  margin-top: 4px;
  border: 0;
  border-top: 1px solid var(--spark-border-light);
  background: transparent;
  color: var(--spark-text-2);
  font-size: var(--spark-font-size-base);
  cursor: pointer;
  font-family: inherit;
}

.mobile-sheet-enter-active,
.mobile-sheet-leave-active {
  transition: opacity var(--spark-dur-page) var(--spark-ease-ios);
}

.mobile-sheet-enter-from,
.mobile-sheet-leave-to {
  opacity: 0;
}
</style>
