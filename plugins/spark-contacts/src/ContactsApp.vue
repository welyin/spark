<!-- 通讯录应用根视图（spark-contacts，communication §4.2 迁移的 ContactsPage 组合）：
     左栏（搜索 + 功能区 + 分组列表）常驻；桌面端「联系人」态为四栏，
     「新的朋友」「标签」占第三/四栏；space 由桥绑定（sdk-host.boundSpaceKey）。
     移动端按视口宽度在列表/详情之间切换（插件内自包含本地导航，无壳层
     导航栈——v1 差距记录在案：壳层移动栈帧转场/系统返回键栈语义不接）。
     v1 边界（A19 遗留）：组织空间「添加成员」邀请流程依赖 organization 域
     SDK 面（未进 sdk.contacts，接口面变更待拍板），组织分支不渲染入口。 -->
<template>
  <section class="contacts-page">
    <!-- 移动端：未选视图整页列表，选中后整页内容（返回经返回栏） -->
    <template v-if="isMobileLayout">
      <!-- 栈1：搜索 + 功能区 + 分组列表 -->
      <div v-if="mobileView.page === 'root'" class="contacts-list">
        <header class="contacts-toolbar">
          <el-input
            v-model="keyword"
            class="contacts-search"
            placeholder="搜索联系人"
            clearable
            :prefix-icon="SearchIcon"
          />
        </header>
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

      <!-- 组内成员列表整页层 -->
      <div v-else-if="mobileView.page === 'group'" class="mobile-stack-layer">
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

      <!-- 联系人资料卡整页层 -->
      <div v-else-if="mobileView.page === 'contact'" class="mobile-stack-layer">
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

      <!-- 新的朋友 / 标签列表整页层 -->
      <div v-else-if="mobileView.page === 'new-friends'" class="mobile-stack-layer">
        <MobileBackBar :title="spaceType === 'org' ? '新的成员' : '新的朋友'" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <NewFriendsPanel
            view="list"
            :requests="spaceData.requests"
            :outgoing="spaceData.outgoing"
            :space-type="spaceType"
            :space-key="spaceKey"
            @resolve="onResolveRequest"
            @retry="onRetryOutgoing"
            @reply="onReplyOutgoing"
            @ask="onAskRequest"
            @open-detail="onOpenRequestDetail"
          />
        </div>
      </div>
      <div v-else-if="mobileView.page === 'tags'" class="mobile-stack-layer">
        <MobileBackBar title="标签" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <TagManager
            view="list"
            :tags="spaceData.tags"
            :space-key="spaceKey"
            :contacts="contacts"
            @view-member="onViewMemberNav"
            @open-tag="onOpenTagDetail"
          />
        </div>
      </div>

      <!-- 申请详情 / 标签成员管理整页层 -->
      <div v-else-if="mobileView.page === 'request-detail'" class="mobile-stack-layer">
        <MobileBackBar :title="requestDetailTitle" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <NewFriendsPanel
            view="detail"
            :initial-key="mobileView.params?.key ?? ''"
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
      <div v-else-if="mobileView.page === 'tag-detail'" class="mobile-stack-layer">
        <MobileBackBar :title="tagDetailTitle" @back="onMobileBack" />
        <div class="mobile-stack-body contacts-mobile-panel">
          <TagManager
            view="detail"
            :initial-tag-id="mobileView.params?.tagId ?? ''"
            :tags="spaceData.tags"
            :space-key="spaceKey"
            :contacts="contacts"
            @view-member="onViewMemberNav"
          />
        </div>
      </div>
    </template>

    <!-- 桌面端：左栏 + 右栏多栏布局（与壳层 ContactsPage 桌面分支同构） -->
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
          <!-- §3：个人空间「添加朋友」；组织空间「添加成员」v1 缺口（organization
               域邀请 API 未进 SDK 面，A19 遗留），组织分支不渲染入口 -->
          <el-button v-if="isPersonal" type="primary" @click="addFriendVisible = true">添加朋友</el-button>
        </header>

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

      <!-- 新的朋友：第三栏申请列表 + 第四栏申请人资料卡 -->
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

      <!-- 标签：第三栏标签列表 + 第四栏成员管理 -->
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

    <!-- 标签成员详情抽屉：复用联系人资料卡（与第四栏同一份 ContactPanel） -->
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
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { Close, Search } from '@element-plus/icons-vue';
import { boundSpaceKey, pluginSpace } from './sdk-host';
import { contactsOf } from './store';
import { isMobileLayout } from './ui-layout';
import { useContactsData } from './components/use-contacts-data';
import { useContactGroups } from './components/use-contact-groups';
import { useContactPanel } from './components/use-contact-panel';
import ContactList from './components/ContactList.vue';
import ContactPanel from './components/ContactPanel.vue';
import GroupPanel from './components/GroupPanel.vue';
import TagManager from './components/TagManager.vue';
import AddFriendDialog from './components/AddFriendDialog.vue';
import NewFriendsPanel from './components/NewFriendsPanel.vue';
import MobileBackBar from './components/MobileBackBar.vue';
import type { ContactItem, RightView } from './components/types';

/** 移动端本地导航帧（插件内自包含；无壳层导航栈——v1 差距记录在案） */
type MobileFrame = {
  page: 'root' | 'group' | 'contact' | 'new-friends' | 'tags' | 'request-detail' | 'tag-detail';
  params?: { groupId?: string; rootId?: string; key?: string; tagId?: string };
};

export default defineComponent({
  name: 'ContactsApp',
  components: {
    ContactList,
    ContactPanel,
    GroupPanel,
    TagManager,
    AddFriendDialog,
    NewFriendsPanel,
    MobileBackBar,
    Close
  },
  setup() {
    const keyword = ref('');
    const rightView = ref<RightView>('contact');
    const addFriendVisible = ref(false);
    // 组织邀请对话框 v1 不实现（InviteMemberDialog 依赖 organization 域 SDK 面）：
    // inviteVisible 恒 false，仅满足 useContactPanel 的上下文形状
    const inviteVisible = ref(false);
    const selectedRootId = ref('');
    /** 当前选中分组：'ungrouped'=未分组（虚拟组），其余为分组 id */
    const activeGroupId = ref('ungrouped');

    const spaceType = computed(() => pluginSpace().type);
    const isPersonal = computed(() => spaceType.value === 'personal');
    const isOrg = computed(() => spaceType.value === 'org');
    const spaceKey = computed(() => boundSpaceKey());
    const spaceData = computed(() => contactsOf(spaceKey.value));
    const currentSpaceOrgId = computed(() => (pluginSpace().type === 'org' ? pluginSpace().id : ''));

    // 数据装载 + 联系人视图合成（个人=朋友；组织=真实成员 + 本地附加资料）
    const { isOrgAdmin, contacts, filteredContacts, searching, refreshOrganizations } = useContactsData({
      isPersonal,
      isOrg,
      spaceKey,
      spaceData,
      currentSpaceOrgId,
      keyword
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
    // 移动端本地导航（插件内自包含栈，替代壳层 mobile-nav）：
    // 「列表 → 组内成员/新的朋友/标签 → 资料卡/申请详情/标签成员管理」逐层整页
    // ------------------------------------------------------------------
    const mobileView = ref<MobileFrame>({ page: 'root' });
    const mobileStack = ref<MobileFrame[]>([]);

    const pushFrame = (frame: MobileFrame) => {
      mobileStack.value = [...mobileStack.value, mobileView.value];
      mobileView.value = frame;
    };
    const onMobileBack = () => {
      const stack = mobileStack.value;
      if (stack.length > 0) {
        mobileView.value = stack[stack.length - 1];
        mobileStack.value = stack.slice(0, -1);
      } else {
        mobileView.value = { page: 'root' };
      }
    };

    /** 第二栏统一列表行选中：桌面原逻辑；移动端按行类型压本地帧 */
    const onSelectRowNav = (id: string) => {
      onSelectRow(id);
      if (!isMobileLayout.value) {
        return;
      }
      if (id === 'new-friends' || id === 'tags') {
        pushFrame({ page: id });
      } else {
        pushFrame({ page: 'group', params: { groupId: id } });
      }
    };

    /** 选中联系人：桌面切右栏资料卡；移动端压入资料卡帧（整页） */
    const onSelectContact = (contact: ContactItem) => {
      openPanel(contact);
      if (isMobileLayout.value) {
        pushFrame({ page: 'contact', params: { rootId: contact.rootId } });
      }
    };

    /** 标签页成员行点击：移动端整页资料卡；桌面保持抽屉原逻辑 */
    const onViewMemberNav = (rootId: string) => {
      if (!isMobileLayout.value) {
        onViewMember(rootId);
        return;
      }
      if (!contacts.value.some((contact) => contact.rootId === rootId)) {
        return;
      }
      selectedRootId.value = rootId;
      pushFrame({ page: 'contact', params: { rootId } });
    };

    /** 新的朋友列表点行（移动端）：压入申请详情帧 */
    const onOpenRequestDetail = (dir: string, id: string) => {
      if (isMobileLayout.value) {
        pushFrame({ page: 'request-detail', params: { key: `${dir}:${id}` } });
      }
    };

    /** 标签列表点行（移动端）：压入标签成员管理帧 */
    const onOpenTagDetail = (tagId: string) => {
      if (isMobileLayout.value) {
        pushFrame({ page: 'tag-detail', params: { tagId } });
      }
    };

    /** 申请详情页返回栏标题：申请人昵称（申请快照），找不到时回退通用文案 */
    const requestDetailTitle = computed(() => {
      if (mobileView.value.page !== 'request-detail') {
        return '申请详情';
      }
      const [dir, id] = (mobileView.value.params?.key ?? '').split(':');
      const list = dir === 'out' ? spaceData.value.outgoing : spaceData.value.requests;
      return list.find((request) => request.id === id)?.nickname ?? '申请详情';
    });

    /** 标签成员管理页返回栏标题：标签名 */
    const tagDetailTitle = computed(() => {
      if (mobileView.value.page !== 'tag-detail') {
        return '标签';
      }
      const tagId = mobileView.value.params?.tagId ?? '';
      return spaceData.value.tags.find((tag) => tag.id === tagId)?.name ?? '标签';
    });

    /** 删除联系人收尾：删除成功后移动端若栈顶是被删联系人的资料卡帧则返回上一层 */
    const onDeleteContactNav = async () => {
      const deletedRootId = selectedRootId.value;
      await onDeleteContact();
      if (
        deletedRootId &&
        !selectedRootId.value &&
        isMobileLayout.value &&
        mobileView.value.page === 'contact' &&
        mobileView.value.params?.rootId === deletedRootId
      ) {
        onMobileBack();
      }
    };

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
      mobileView,
      onSelectRowNav,
      onSelectContact,
      onMobileBack,
      onOpenRequestDetail,
      onOpenTagDetail,
      requestDetailTitle,
      tagDetailTitle
    };
  }
});
</script>

<style>
/* 迁移自壳层 contacts 页样式（.contacts-page 作用域）；tokens 变量自包含 */
@import './styles/tokens.css';
@import './styles/pages/contacts.css';
</style>
