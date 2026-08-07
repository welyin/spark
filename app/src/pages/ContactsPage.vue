<!-- 通讯录页（ui-contacts）：左栏（搜索 + 功能区 + 分组列表）常驻；
     「联系人」态为四栏——第二栏分组、第三栏组内联系人、第四栏资料卡；
     「新的朋友」「标签」占第三/四栏（各自 fragment 双栏）；
     个人空间=朋友（mock），组织空间=成员（真实 listMine + 本地附加资料 mock） -->
<template>
  <section class="contacts-page">
    <!-- 移动端（波次 2/3）：整页 + 导航栈，栈帧切换经 MobilePageTransition 滑动转场（微信式）——
         栈1 列表（搜索 + 功能区 + 分组）；栈2 组内成员/新的朋友/标签；栈2-3 联系人资料卡 -->
    <MobilePageTransition v-if="isMobileLayout" :tab="MOBILE_TAB">
      <!-- 栈1：搜索 + 功能区 + 分组列表 -->
      <div v-if="mobileFrame.page === 'root'" class="contacts-list">
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
          @select="onSelectContact"
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
          @select="onSelectRowNav"
        />
      </div>

      <!-- 栈2：组内成员列表整页层 -->
      <div v-else-if="mobileFrame.page === 'group'" class="mobile-stack-layer">
        <MobileBackBar :title="activeGroupName" @back="onMobileBack" />
        <div class="mobile-stack-body">
          <ContactList
            :items="groupMembers"
            :active-root-id="selectedRootId"
            group-by-letter
            empty-text="该分组暂无联系人"
            @select="onSelectContact"
          />
        </div>
      </div>

      <!-- 联系人资料卡整页层（搜索直达为栈2，组内点开为栈3） -->
      <div v-else-if="mobileFrame.page === 'contact'" class="mobile-stack-layer">
        <MobileBackBar :title="selectedContact ? selectedContact.displayName : '联系人资料'" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-detail">
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
            @delete="onDeleteContactNav"
            @send-message="onSendMessage"
            @add-as-friend="onAddAsFriend"
          />
          <el-empty v-else class="contacts-detail-empty" :image-size="110" description="联系人不存在或已删除" />
        </div>
      </div>

      <!-- 新的朋友 / 标签整页层（面板内部双栏纵排，见 contacts.css 波次 2 媒体查询） -->
      <div v-else-if="mobileFrame.page === 'new-friends'" class="mobile-stack-layer">
        <MobileBackBar :title="spaceType === 'org' ? '新的成员' : '新的朋友'" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <NewFriendsPanel
            :requests="spaceData.requests"
            :outgoing="spaceData.outgoing"
            :space-type="spaceType"
            :space-key="spaceKey"
            @resolve="onResolveRequest"
            @retry="onRetryOutgoing"
            @reply="onReplyOutgoing"
            @ask="onAskRequest"
          />
        </div>
      </div>
      <div v-else-if="mobileFrame.page === 'tags'" class="mobile-stack-layer">
        <MobileBackBar title="标签" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <TagManager
            :tags="spaceData.tags"
            :space-key="spaceKey"
            :contacts="contacts"
            @view-member="onViewMemberNav"
          />
        </div>
      </div>
    </MobilePageTransition>

    <!-- 桌面端（≥769px 渲染逻辑不变）：左栏 + 右栏多栏布局 -->
    <template v-else>
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
          @select="onSelectContact"
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
          @select="onSelectRowNav"
        />
      </div>

      <!-- 桌面端右栏：新的朋友：第三栏申请列表 + 第四栏申请人资料卡（四栏结构，与个人设置模块同构） -->
      <NewFriendsPanel
        v-if="rightView === 'new-friends'"
        :requests="spaceData.requests"
        :outgoing="spaceData.outgoing"
        :space-type="spaceType"
        :space-key="spaceKey"
        @resolve="onResolveRequest"
        @retry="onRetryOutgoing"
        @reply="onReplyOutgoing"
        @ask="onAskRequest"
      />

      <!-- 标签：第三栏标签列表 + 第四栏成员管理（四栏结构，同新的朋友） -->
      <TagManager
        v-else-if="rightView === 'tags'"
        :tags="spaceData.tags"
        :space-key="spaceKey"
        :contacts="contacts"
        @view-member="onViewMemberNav"
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
            @select="onSelectContact"
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
            @delete="onDeleteContactNav"
            @send-message="onSendMessage"
            @add-as-friend="onAddAsFriend"
          />
          <el-empty v-else class="contacts-detail-empty" :image-size="110" description="选择联系人查看资料" />
        </div>
      </template>
    </template>

    <!-- 标签成员详情抽屉：复用联系人资料卡（与第四栏同一份 ContactPanel）；
         全 app 抽屉统一：无头部小标题（资料卡名字即标题），右上角自定义关闭 -->
    <el-drawer v-model="drawerVisible" :with-header="false" :size="isMobileLayout ? '100%' : 440" class="app-drawer">
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
          @delete="onDeleteContactNav"
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
import { isMobileLayout } from '../stores/ui-layout';
import { currentPage, popPage, pushPage, resetStack } from '../stores/mobile-nav';
import { pendingAddContact, consumePendingAddContact } from '../stores/pending-add-contact';
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
import MobileBackBar from '../components/MobileBackBar.vue';
import MobilePageTransition from '../components/MobilePageTransition.vue';
import type { ContactItem, RightView } from '../components/contacts/types';

/** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
const MOBILE_TAB = 'contacts';

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
    MobileBackBar,
    MobilePageTransition,
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

    // 切空间：重置视图状态并按需重取组织成员；移动端同步回栈底（联系人不跨空间）
    watch(spaceKey, () => {
      keyword.value = '';
      rightView.value = 'contact';
      selectedRootId.value = '';
      activeGroupId.value = 'ungrouped';
      resetStack(MOBILE_TAB);
      void refreshOrganizations();
    });

    // 顶栏「+」菜单的添加朋友/添加成员请求（pending-add-contact）：切到本 tab 挂载时（immediate）
    // 或停留本页时消费，打开对应添加对话框（复用现有添加朋友/成员邀请流程）
    watch(
      pendingAddContact,
      (kind) => {
        if (!kind) {
          return;
        }
        consumePendingAddContact();
        if (kind === 'friend') {
          addFriendVisible.value = true;
        } else if (isOrg.value) {
          inviteVisible.value = true;
        }
      },
      { immediate: true }
    );

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
      onRetryOutgoing,
      onReplyOutgoing,
      onAskRequest,
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

    // ------------------------------------------------------------------
    // 移动端导航栈（波次 2）：窄屏下「列表 → 组内成员 → 资料卡」逐层整页，
    // 新的朋友/标签为列表的下一层整页；桌面端以下逻辑均不触发（isMobileLayout 恒 false）
    // ------------------------------------------------------------------
    const mobileFrame = computed(() => currentPage(MOBILE_TAB));

    /** 第二栏统一列表行选中：桌面原逻辑；移动端按行类型压栈（功能行→对应面板页，分组行→组内成员页） */
    const onSelectRowNav = (id: string) => {
      onSelectRow(id);
      if (!isMobileLayout.value) {
        return;
      }
      if (id === 'new-friends' || id === 'tags') {
        pushPage(MOBILE_TAB, id);
      } else {
        pushPage(MOBILE_TAB, 'group', { groupId: id });
      }
    };

    /** 选中联系人：桌面切右栏资料卡；移动端压入资料卡栈帧（整页） */
    const onSelectContact = (contact: ContactItem) => {
      openPanel(contact);
      if (isMobileLayout.value) {
        pushPage(MOBILE_TAB, 'contact', { rootId: contact.rootId });
      }
    };

    /** 标签页成员行点击：移动端只走导航栈（整页资料卡，不再叠加抽屉）；桌面保持抽屉原逻辑 */
    const onViewMemberNav = (rootId: string) => {
      if (!isMobileLayout.value) {
        onViewMember(rootId);
        return;
      }
      if (!contacts.value.some((contact) => contact.rootId === rootId)) {
        return;
      }
      selectedRootId.value = rootId;
      pushPage(MOBILE_TAB, 'contact', { rootId });
    };

    /** 删除联系人收尾：删除成功（selectedRootId 已被清空；取消/失败保持原值）后，
        移动端若栈顶是被删联系人的资料卡帧则弹出，避免返回残留空页 */
    const onDeleteContactNav = async () => {
      const deletedRootId = selectedRootId.value;
      await onDeleteContact();
      if (
        deletedRootId &&
        !selectedRootId.value &&
        isMobileLayout.value &&
        currentPage(MOBILE_TAB).page === 'contact' &&
        currentPage(MOBILE_TAB).params?.rootId === deletedRootId
      ) {
        popPage(MOBILE_TAB);
      }
    };

    /** 返回栏：弹出栈顶回上一栏 */
    const onMobileBack = () => popPage(MOBILE_TAB);

    // 栈顶帧变化（重进 tab 按栈恢复 / 返回 pop / 重按 tab 复位）时同步本地视图状态
    watch(
      [mobileFrame, isMobileLayout],
      ([frame, mobile]) => {
        if (!mobile) {
          return;
        }
        if (frame.page === 'contact') {
          rightView.value = 'contact';
          selectedRootId.value = frame.params?.rootId ?? '';
        } else if (frame.page === 'group') {
          rightView.value = 'contact';
          selectedRootId.value = '';
          activeGroupId.value = frame.params?.groupId ?? 'ungrouped';
        } else if (frame.page === 'new-friends' || frame.page === 'tags') {
          rightView.value = frame.page;
          selectedRootId.value = '';
        } else {
          rightView.value = 'contact';
          selectedRootId.value = '';
        }
      },
      { immediate: true }
    );

    // 组合内直达（全局搜索「打开联系人资料」等只写 selectedRootId）：移动端按选中补资料卡栈帧
    watch(selectedRootId, (rootId) => {
      if (!isMobileLayout.value || !rootId) {
        return;
      }
      pushPage(MOBILE_TAB, 'contact', { rootId });
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
      onViewMemberNav,
      noopAsync,
      refreshOrganizations,
      onSelectGroup,
      onSelectRow,
      openPanel,
      onSaveProfile,
      onSetBlocked,
      onSendMessage,
      onDeleteContactNav,
      onAddFriendSubmit,
      onResolveRequest,
      onRetryOutgoing,
      onReplyOutgoing,
      onAskRequest,
      onAddAsFriend,
      onCreateTagReturn,
      isMobileLayout,
      mobileFrame,
      MOBILE_TAB,
      onSelectRowNav,
      onSelectContact,
      onMobileBack
    };
  }
});
</script>

<style src="../styles/pages/contacts.css"></style>
