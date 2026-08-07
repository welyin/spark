<!-- 移动端左滑侧边栏（Android 前端改造，企业微信风格）：
     遮罩 + 左滑抽屉。打开时仅可点击抽屉内容，点击遮罩任意位置收回。
     上部=当前用户（头像+昵称）；中部=空间列表（个人空间 + 组织列表，未读红点）；
     加入/创建 合并为一个入口，点击从底部弹出上滑菜单选择「创建组织」或「加入组织」；
     最后一行=设置（点击关抽屉并切到设置 tab）。退出登录/切换账号已下放至设置页最底层。 -->
<template>
  <Teleport to="body">
    <Transition name="mobile-drawer">
      <div v-if="visible" class="mobile-drawer-root">
        <!-- 遮罩：点击任意位置收回（抽屉内容区点击不冒泡） -->
        <div class="mobile-drawer-mask" @click="close" />

        <aside class="mobile-drawer" @click.stop>
          <!-- 上部：当前用户 -->
          <div class="mobile-drawer-user">
            <UserAvatar
              v-if="isPersonal"
              :root-id="personalSource.seed"
              :nickname="personalSource.name"
              :avatar="personalSource.image"
              :size="40"
            />
            <OrgAvatar v-else :org-id="currentOrgId" :name="currentSpaceName" :size="40" />
            <div class="mobile-drawer-user-info">
              <b>{{ userDisplayName }}</b>
              <span>{{ isPersonal ? '个人空间' : currentSpaceName }}</span>
            </div>
          </div>

          <!-- 中部：空间列表 -->
          <nav class="mobile-drawer-spaces">
            <button
              type="button"
              class="mobile-drawer-item"
              :class="{ active: isPersonal }"
              @click="selectPersonal"
            >
              <span class="mobile-drawer-item-avatar" :class="{ 'has-dot': personalNotification }">
                <UserAvatar
                  :root-id="personalSource.seed"
                  :nickname="personalSource.name"
                  :avatar="personalSource.image"
                  :size="40"
                />
              </span>
              <span class="mobile-drawer-item-label">个人空间</span>
              <el-icon v-if="isPersonal" class="mobile-drawer-item-check"><Check /></el-icon>
            </button>

            <button
              v-for="org in organizations"
              :key="org.orgId"
              type="button"
              class="mobile-drawer-item"
              :class="{ active: !isPersonal && currentOrgId === org.orgId }"
              @click="selectOrg(org.orgId)"
            >
              <span class="mobile-drawer-item-avatar" :class="{ 'has-dot': orgNotification(org.orgId) }">
                <OrgAvatar :org-id="org.orgId" :name="org.name" :size="40" />
              </span>
              <span class="mobile-drawer-item-label">{{ org.name }}</span>
              <el-icon v-if="!isPersonal && currentOrgId === org.orgId" class="mobile-drawer-item-check"><Check /></el-icon>
            </button>
          </nav>

          <!-- 底部：加入/创建组织（设置上方，企业微信式） -->
          <div class="mobile-drawer-footer">
            <button type="button" class="mobile-drawer-item mobile-drawer-action" @click="openJoinCreateSheet">
              <el-icon :size="17" :style="{ color: '#34c19b' }"><Plus /></el-icon>
              <span class="mobile-drawer-item-label">加入/创建组织</span>
            </button>

            <!-- 「设置」与上方「加入/创建」同为底部操作条目，统一用 mobile-drawer-action 弱色
                 （原为默认 text-1，比相邻条目偏白/偏亮，看着不一致） -->
            <button type="button" class="mobile-drawer-item mobile-drawer-action" @click="goSettings">
              <el-icon :size="17" :style="{ color: '#64748b' }"><Setting /></el-icon>
              <span class="mobile-drawer-item-label">设置</span>
            </button>
          </div>
        </aside>

        <!-- 加入/创建 上滑菜单 -->
        <Transition name="mobile-sheet">
          <div v-if="joinCreateSheetVisible" class="mobile-sheet-root" @click="joinCreateSheetVisible = false">
            <div class="mobile-sheet" @click.stop>
              <button type="button" class="mobile-sheet-item" @click="openCreateDialog">
                <el-icon :size="18" :style="{ color: '#00b8a9' }"><OfficeBuilding /></el-icon>
                <div class="mobile-sheet-item-text">
                  <b>创建组织</b>
                  <span>创建一个新的组织空间</span>
                </div>
              </button>
              <button type="button" class="mobile-sheet-item" @click="openJoinDialog">
                <el-icon :size="18" :style="{ color: '#3296fa' }"><Connection /></el-icon>
                <div class="mobile-sheet-item-text">
                  <b>加入组织</b>
                  <span>通过邀请码加入已有组织</span>
                </div>
              </button>
              <button type="button" class="mobile-sheet-cancel" @click="joinCreateSheetVisible = false">取消</button>
            </div>
          </div>
        </Transition>
      </div>
    </Transition>

    <!-- 创建/加入对话框（复用桌面端组件；挂在 Teleport 内保持层级） -->
    <CreateOrgDialog
      v-model="createDialogVisible"
      :creating="creating"
      @submit="createOrganization"
    />
    <JoinOrgDialog v-model="joinDialogVisible" :joining="joining" @submit="acceptInvite" />
  </Teleport>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { Check, Connection, OfficeBuilding, Plus, Setting } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId, switchToOrg, switchToPersonal } from '../stores/current-space';
import { organizations, refreshOrganizations as refreshOrgMembership } from '../stores/org-membership';
import { hasSpaceNotification } from '../stores/space-notifications';
import { currentUser } from '../stores/current-user';
import { setOrgAvatar } from '../stores/org-avatars';
import { personalAvatarSource } from '../stores/avatar-sources';
import { spaceKeyOf } from '../mock/contacts';
import UserAvatar from './UserAvatar.vue';
import OrgAvatar from './OrgAvatar.vue';
import CreateOrgDialog from './org/CreateOrgDialog.vue';
import JoinOrgDialog from './org/JoinOrgDialog.vue';
import type { CreateForm } from './org/types';

export default defineComponent({
  name: 'MobileSpaceDrawer',
  components: {
    UserAvatar,
    OrgAvatar,
    CreateOrgDialog,
    JoinOrgDialog,
    Check,
    Connection,
    OfficeBuilding,
    Plus,
    Setting
  },
  props: {
    /** 抽屉可见性（v-model） */
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue', 'open-settings'],
  setup(props, { emit }) {
    const visible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });
    const close = () => {
      visible.value = false;
    };

    // 加入/创建 上滑菜单
    const joinCreateSheetVisible = ref(false);
    const createDialogVisible = ref(false);
    const creating = ref(false);
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

    const userDisplayName = computed(() => currentUser.nickname || '未命名用户');

    const personalNotification = computed(() => hasSpaceNotification(spaceKeyOf({ type: 'personal' })));
    const orgNotification = (orgId: string) => hasSpaceNotification(spaceKeyOf({ type: 'org', orgId }));

    const refreshOrganizations = async () => {
      try {
        await refreshOrgMembership();
      } catch {
        // 读取失败保留旧数据
      }
    };

    const openJoinCreateSheet = () => {
      joinCreateSheetVisible.value = true;
    };

    const openCreateDialog = () => {
      joinCreateSheetVisible.value = false;
      createDialogVisible.value = true;
    };

    const openJoinDialog = () => {
      joinCreateSheetVisible.value = false;
      joinDialogVisible.value = true;
    };

    const selectPersonal = () => {
      close();
      switchToPersonal();
    };

    const selectOrg = (orgId: string) => {
      close();
      switchToOrg(orgId);
    };

    const goSettings = () => {
      close();
      emit('open-settings');
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
          description: form.description,
          avatar: form.avatar || undefined
        });
        if (form.avatar) {
          setOrgAvatar(created.orgId, form.avatar);
        }
        ElMessage.success(`组织已创建：${created.name}`);
        createDialogVisible.value = false;
        await refreshOrganizations();
        switchToOrg(created.orgId);
      } catch (error) {
        ElMessage.error(`创建组织失败：${error}`);
      } finally {
        creating.value = false;
      }
    };

    return {
      visible,
      close,
      organizations,
      isPersonal,
      currentOrgId,
      currentSpaceName,
      userDisplayName,
      joinCreateSheetVisible,
      createDialogVisible,
      creating,
      joinDialogVisible,
      joining,
      openJoinCreateSheet,
      openCreateDialog,
      openJoinDialog,
      selectPersonal,
      selectOrg,
      goSettings,
      acceptInvite,
      createOrganization,
      personalNotification,
      orgNotification,
      personalSource: computed(() => personalAvatarSource())
    };
  }
});
</script>

<style scoped>
.mobile-drawer-root {
  position: fixed;
  inset: 0;
  z-index: 2000;
}

.mobile-drawer-mask {
  position: absolute;
  inset: 0;
  background: rgba(0, 0, 0, 0.35);
}

.mobile-drawer {
  position: absolute;
  /* 顶到屏幕顶（占用系统状态栏区域，顶部多出一块同色空白）；
     内部元素视觉位置由 .mobile-drawer-user 的 padding-top 叠加 safe-area 保持 */
  top: 0;
  left: 0;
  bottom: env(safe-area-inset-bottom, 0px);
  width: min(300px, 82vw);
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-card);
  box-shadow: 4px 0 16px rgba(0, 0, 0, 0.12);
}

/* 上部：当前用户（padding-top 叠加状态栏安全区：抽屉面板 top:0 占用状态栏后，
   用户行视觉位置与原先避开状态栏时一致，上方留出一块空白区） */
.mobile-drawer-user {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: calc(20px + env(safe-area-inset-top, 0px)) 16px 14px;
  border-bottom: 1px solid var(--spark-border-light);
}

.mobile-drawer-user-info {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mobile-drawer-user-info b {
  font-size: 15px;
  font-weight: 600;
  color: var(--spark-text-1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mobile-drawer-user-info span {
  font-size: 13px;
  color: var(--spark-text-3);
}

/* 中部：空间列表 */
.mobile-drawer-spaces {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 8px;
}

/* 底部：设置 */
.mobile-drawer-footer {
  flex-shrink: 0;
  border-top: 1px solid var(--spark-border-light);
  padding: 8px;
}

.mobile-drawer-item {
  display: flex;
  align-items: center;
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  /* 行参数（58px 行高 / 0 16px 内边距 / 12px 间距 / 16px 主文字）统一在
     mine.css 移动端媒体块按通讯录 request-item 基准收敛，此处不再重复定义 */
  border-radius: var(--spark-radius-m);
  text-align: left;
}

.mobile-drawer-item:hover {
  background: var(--spark-bg-hover);
}

.mobile-drawer-item.active {
  background: var(--spark-primary-light);
}

.mobile-drawer-item.active .mobile-drawer-item-label {
  color: var(--spark-primary);
}

.mobile-drawer-item-avatar {
  position: relative;
  flex-shrink: 0;
  display: inline-flex;
}

.mobile-drawer-item-avatar.has-dot::after {
  content: '';
  position: absolute;
  top: -2px;
  right: -2px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 2px solid var(--spark-bg-card);
  background: var(--spark-danger);
}

.mobile-drawer-item-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mobile-drawer-item-check {
  color: var(--spark-primary);
  flex-shrink: 0;
}

.mobile-drawer-action {
  color: var(--spark-text-2);
  margin-top: 4px;
}

/* ---- 加入/创建 上滑菜单 ---- */
.mobile-sheet-root {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: flex-end;
  justify-content: center;
  background: rgba(0, 0, 0, 0.35);
  z-index: 2010;
}

.mobile-sheet {
  width: 100%;
  max-width: 480px;
  background: var(--spark-bg-card);
  border-radius: 16px 16px 0 0;
  padding: 10px 12px calc(10px + env(safe-area-inset-bottom, 0px));
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mobile-sheet-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  padding: 12px 10px;
  border-radius: var(--spark-radius-m);
  text-align: left;
  color: var(--spark-text-1);
}

.mobile-sheet-item:hover {
  background: var(--spark-bg-hover);
}

.mobile-sheet-item-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mobile-sheet-item-text b {
  font-size: 16px;
  font-weight: 500;
}

.mobile-sheet-item-text span {
  font-size: 13px;
  color: var(--spark-text-3);
}

.mobile-sheet-cancel {
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  font-size: 16px;
  color: var(--spark-text-2);
  padding: 12px 10px;
  border-radius: var(--spark-radius-m);
  border-top: 1px solid var(--spark-border-light);
}

/* ---- 过渡动画 ---- */
.mobile-drawer-enter-active,
.mobile-drawer-leave-active {
  transition: opacity 220ms ease;
}

.mobile-drawer-enter-active .mobile-drawer,
.mobile-drawer-leave-active .mobile-drawer {
  transition: transform 260ms cubic-bezier(0.25, 0.46, 0.45, 0.94);
}

.mobile-drawer-enter-from,
.mobile-drawer-leave-to {
  opacity: 0;
}

.mobile-drawer-enter-from .mobile-drawer,
.mobile-drawer-leave-to .mobile-drawer {
  transform: translateX(-100%);
}

.mobile-sheet-enter-active,
.mobile-sheet-leave-active {
  transition: opacity 200ms ease;
}

.mobile-sheet-enter-active .mobile-sheet,
.mobile-sheet-leave-active .mobile-sheet {
  transition: transform 220ms cubic-bezier(0.25, 0.46, 0.45, 0.94);
}

.mobile-sheet-enter-from,
.mobile-sheet-leave-to {
  opacity: 0;
}

.mobile-sheet-enter-from .mobile-sheet,
.mobile-sheet-leave-to .mobile-sheet {
  transform: translateY(100%);
}
</style>
