<!-- 通讯录页（ui-contacts）：左栏（搜索 + 功能区 + 分组列表）常驻；
     「联系人」态为四栏——第二栏分组、第三栏组内联系人、第四栏资料卡；
     「新的朋友」「标签」占第三/四栏（各自 fragment 双栏）；
     个人空间=朋友（mock），组织空间=成员（真实 listMine + 本地附加资料 mock） -->
<template>
  <section class="contacts-page">
    <!-- 左栏：搜索 + 功能区 + 分组列表（右栏切换时保持不变） -->
    <div class="contacts-list">
      <header class="contacts-toolbar">
        <el-input
          v-model="keyword"
          class="contacts-search"
          placeholder="搜索联系人"
          clearable
          :prefix-icon="SearchIcon"
        />
        <!-- §3：个人空间「添加朋友」；组织空间仅管理员显示「添加成员」（复用组织邀请流程） -->
        <el-button v-if="isPersonal" type="primary" @click="addFriendVisible = true">添加朋友</el-button>
        <el-button v-else-if="isOrgAdmin" type="primary" @click="inviteVisible = true">添加成员</el-button>
      </header>

      <!-- 搜索态：扁平结果列表；非搜索态：统一列表（功能区 + 分组，共享选中态与滚动） -->
      <ContactList
        v-if="searching"
        :items="filteredContacts"
        :active-root-id="rightView === 'contact' ? selectedRootId : ''"
        :keyword="keyword"
        :empty-text="isPersonal ? '无匹配的朋友' : '无匹配的成员'"
        @select="openPanel"
      />
      <GroupPanel
        v-else
        :mode="isPersonal ? 'personal' : 'org'"
        :space-key="spaceKey"
        :groups="spaceData.groups"
        :group-tree="spaceData.groupTree"
        :counts="groupCounts"
        :pending-count="pendingCount"
        :admin-count="adminCount"
        :active-id="rightView === 'contact' ? activeGroupId : rightView"
        :can-edit-structure="isPersonal || isOrgAdmin"
        @select="onSelectRow"
      />
    </div>

    <!-- 新的朋友：第三栏申请列表 + 第四栏申请人资料卡（四栏结构，与个人设置模块同构） -->
    <NewFriendsPanel
      v-if="rightView === 'new-friends'"
      :requests="spaceData.requests"
      :outgoing="spaceData.outgoing"
      :space-type="spaceType"
      :space-key="spaceKey"
      @resolve="onResolveRequest"
      @view-contact="onViewRequestContact"
      @retry="onRetryOutgoing"
      @reply="onReplyOutgoing"
    />

    <!-- 标签：第三栏标签列表 + 第四栏成员管理（四栏结构，同新的朋友） -->
    <TagManager
      v-else-if="rightView === 'tags'"
      :tags="spaceData.tags"
      :space-key="spaceKey"
      :contacts="contacts"
      @view-member="onViewMember"
    />

    <!-- 联系人（默认，§5）：第三栏组内联系人 + 第四栏资料卡 -->
    <template v-else>
      <div class="contacts-request-list">
        <h2 class="contacts-request-title">{{ activeGroupName }}</h2>
        <ContactList
          :items="groupMembers"
          :active-root-id="selectedRootId"
          group-by-letter
          empty-text="该分组暂无联系人"
          @select="openPanel"
        />
      </div>
      <div class="contacts-detail">
        <ContactPanel
          v-if="selectedContact && selectedProfile"
          :key="selectedContact.rootId"
          :contact="selectedContact"
          :space-type="spaceType"
          :is-admin="isOrgAdmin"
          :profile="selectedProfile"
          :all-tags="spaceData.tags"
          :group-options="groupOptions"
          :on-create-tag="onCreateTagReturn"
          @save-profile="onSaveProfile"
          @set-blocked="onSetBlocked"
          @delete="onDeleteContact"
          @send-message="onSendMessage"
          @add-as-friend="onAddAsFriend"
        />
        <el-empty v-else class="contacts-detail-empty" :image-size="110" description="选择联系人查看资料" />
      </div>
    </template>

    <!-- 标签成员详情抽屉：复用联系人资料卡（与第四栏同一份 ContactPanel）；
         全 app 抽屉统一：无头部小标题（资料卡名字即标题），右上角自定义关闭 -->
    <el-drawer v-model="drawerVisible" :with-header="false" size="440" class="app-drawer">
      <button type="button" class="app-drawer-close" title="关闭" @click="drawerVisible = false">
        <el-icon :size="16"><Close /></el-icon>
      </button>
      <div class="app-drawer-body">
        <ContactPanel
          v-if="selectedContact && selectedProfile"
          :key="`drawer-${selectedContact.rootId}`"
          :contact="selectedContact"
          :space-type="spaceType"
          :is-admin="isOrgAdmin"
          :profile="selectedProfile"
          :all-tags="spaceData.tags"
          :group-options="groupOptions"
          :on-create-tag="onCreateTagReturn"
          @save-profile="onSaveProfile"
          @set-blocked="onSetBlocked"
          @delete="onDeleteContact"
          @send-message="onSendMessage"
          @add-as-friend="onAddAsFriend"
        />
      </div>
    </el-drawer>

    <AddFriendDialog v-model="addFriendVisible" @submit="onAddFriendSubmit" />
    <InviteMemberDialog
      v-if="isOrg"
      v-model="inviteVisible"
      :org-id="currentSpaceOrgId"
      :before-write="noopAsync"
      :on-invited="refreshOrganizations"
    />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { Close, Search } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId, currentSpaceType } from '../stores/current-space';
import { contactsOf, spaceKeyOf } from '../mock/contacts';
import { useContactsData } from '../components/contacts/use-contacts-data';
import { useContactGroups } from '../components/contacts/use-contact-groups';
import { useContactPanel } from '../components/contacts/use-contact-panel';
import ContactList from '../components/contacts/ContactList.vue';
import ContactPanel from '../components/contacts/ContactPanel.vue';
import GroupPanel from '../components/contacts/GroupPanel.vue';
import TagManager from '../components/contacts/TagManager.vue';
import AddFriendDialog from '../components/contacts/AddFriendDialog.vue';
import NewFriendsPanel from '../components/contacts/NewFriendsPanel.vue';
import InviteMemberDialog from '../components/org/InviteMemberDialog.vue';
import type { RightView } from '../components/contacts/types';

export default defineComponent({
  name: 'ContactsPage',
  components: {
    ContactList,
    ContactPanel,
    GroupPanel,
    TagManager,
    AddFriendDialog,
    NewFriendsPanel,
    InviteMemberDialog,
    Close
  },
  setup() {
    const keyword = ref('');
    const rightView = ref<RightView>('contact');
    const addFriendVisible = ref(false);
    const inviteVisible = ref(false);
    const selectedRootId = ref('');
    /** 当前选中分组：'ungrouped'=未分组（虚拟组），其余为分组 id（拉黑者不进通讯录，黑名单在个人设置管理） */
    const activeGroupId = ref('ungrouped');

    const spaceType = currentSpaceType;
    const isPersonal = computed(() => spaceType.value === 'personal');
    const isOrg = computed(() => spaceType.value === 'org');
    const spaceKey = computed(() => spaceKeyOf(currentSpace.value));
    const spaceData = computed(() => contactsOf(spaceKey.value));

    const noopAsync = async () => {};

    // 数据装载 + 联系人视图合成（个人=mock 朋友；组织=真实成员 + 本地附加资料）
    const { isOrgAdmin, contacts, filteredContacts, searching, refreshOrganizations } = useContactsData({
      isPersonal,
      isOrg,
      spaceKey,
      spaceData,
      currentSpaceOrgId,
      keyword
    });

    // 切空间：重置视图状态并按需重取组织成员
    watch(spaceKey, () => {
      keyword.value = '';
      rightView.value = 'contact';
      selectedRootId.value = '';
      activeGroupId.value = 'ungrouped';
      void refreshOrganizations();
    });

    // 分组：第二栏分组列表 -> 第三栏组内成员（个人扁平 / 组织树）
    const {
      groupOptions,
      groupCounts,
      activeGroupName,
      groupMembers,
      pendingCount,
      adminCount,
      onSelectGroup,
      onSelectRow
    } = useContactGroups({ isPersonal, spaceKey, spaceData, contacts, activeGroupId, rightView });

    // 第四栏资料卡 + 申请处理 + 标签「新建并选中」入口
    const {
      selectedContact,
      selectedProfile,
      drawerVisible,
      onViewMember,
      openPanel,
      onSaveProfile,
      onSetBlocked,
      onSendMessage,
      onDeleteContact,
      onAddFriendSubmit,
      onResolveRequest,
      onViewRequestContact,
      onRetryOutgoing,
      onReplyOutgoing,
      onAddAsFriend,
      onCreateTagReturn
    } = useContactPanel({
      isPersonal,
      isOrg,
      isOrgAdmin,
      spaceKey,
      currentSpaceOrgId,
      contacts,
      searching,
      groupOptions,
      selectedRootId,
      rightView,
      activeGroupId,
      addFriendVisible,
      inviteVisible,
      refreshOrganizations
    });

    return {
      SearchIcon: Search,
      keyword,
      rightView,
      addFriendVisible,
      inviteVisible,
      spaceType,
      isPersonal,
      isOrg,
      isOrgAdmin,
      currentSpaceOrgId,
      spaceKey,
      spaceData,
      contacts,
      searching,
      filteredContacts,
      activeGroupId,
      groupOptions,
      groupCounts,
      activeGroupName,
      groupMembers,
      pendingCount,
      adminCount,
      selectedRootId,
      selectedContact,
      selectedProfile,
      drawerVisible,
      onViewMember,
      noopAsync,
      refreshOrganizations,
      onSelectGroup,
      onSelectRow,
      openPanel,
      onSaveProfile,
      onSetBlocked,
      onSendMessage,
      onDeleteContact,
      onAddFriendSubmit,
      onResolveRequest,
      onViewRequestContact,
      onRetryOutgoing,
      onReplyOutgoing,
      onAddAsFriend,
      onCreateTagReturn
    };
  }
});
</script>

<style src="../styles/pages/contacts.css"></style>
