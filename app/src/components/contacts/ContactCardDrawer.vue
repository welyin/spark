<!-- 联系人资料卡抽屉（聊天窗头像/聊天头点击弹出）：
     按 rootId + spaceKey 就地解析联系人（个人=朋友记录；组织=org-membership 缓存成员），
     复用通讯录同一份 ContactPanel 与动作收口（use-contact-actions / use-delete-contact），
     不另造资料卡。联系人在展示期间被删除（朋友被删/成员被移出）时自动收起抽屉。 -->
<template>
  <!-- 全 app 抽屉统一：无头部小标题（资料卡名字即标题），右上角自定义关闭 -->
  <el-drawer :model-value="modelValue" :with-header="false" size="440" class="app-drawer" @update:model-value="setVisible">
    <button type="button" class="app-drawer-close" title="关闭" @click="setVisible(false)">
      <el-icon :size="16"><Close /></el-icon>
    </button>
    <div class="app-drawer-body">
      <ContactPanel
        v-if="contact && profile"
        :key="`card-${contact.rootId}`"
        :contact="contact"
        :space-type="spaceType"
        :is-admin="isOrgAdmin"
        :profile="profile"
        :all-tags="allTags"
        :group-options="groupOptions"
        :on-create-tag="createTag"
        @save-profile="saveProfile"
        @set-blocked="setBlocked"
        @delete="onDeleteContact"
        @send-message="sendMessage"
        @add-as-friend="onAddAsFriend"
      />
      <!-- 朋友已被删除 / 组织成员不存在（含陌生人）时的占位 -->
      <el-empty v-else :image-size="110" description="无法查看该联系人的资料" />
    </div>
  </el-drawer>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { Close } from '@element-plus/icons-vue';
import { findOrg, isAdmin, refreshOrganizations } from '../../stores/org-membership';
import { contactsOf, friendOf, profileOf, sendFriendRequest, type ContactProfile } from '../../mock/contacts';
import ContactPanel from './ContactPanel.vue';
import { friendContactItem, orgGroupOptions, orgMemberContactItem } from './contact-item';
import { useContactActions } from './use-contact-actions';
import { useDeleteContact } from './use-delete-contact';
import type { ContactItem, GroupOption } from './types';

export default defineComponent({
  name: 'ContactCardDrawer',
  components: { ContactPanel, Close },
  props: {
    modelValue: { type: Boolean, required: true },
    /** 目标联系人 rootId（消息 senderId / 会话 peerId；'me' 由调用方换算为 currentUser.rootId） */
    rootId: { type: String, required: true },
    /** 所在空间 key：'personal' 或 'org:<orgId>' */
    spaceKey: { type: String, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const setVisible = (visible: boolean) => emit('update:modelValue', visible);

    const spaceType = computed<'personal' | 'org'>(() => (props.spaceKey === 'personal' ? 'personal' : 'org'));
    const isPersonal = computed(() => spaceType.value === 'personal');
    const orgId = computed(() => (isPersonal.value ? '' : props.spaceKey.slice('org:'.length)));
    const spaceKeyRef = computed(() => props.spaceKey);

    // 联系人解析：个人=朋友记录单点映射；组织=成员缓存 + 共享三分支构造（与通讯录列表同一份）。
    // 不做拉黑过滤（被拉黑者也要能进资料卡移出黑名单）
    const contact = computed<ContactItem | null>(() => {
      if (!props.rootId) {
        return null;
      }
      if (isPersonal.value) {
        const friend = friendOf('personal', props.rootId);
        return friend ? friendContactItem(friend) : null;
      }
      const member = findOrg(orgId.value)?.members.find((item) => item.rootId === props.rootId);
      return member ? orgMemberContactItem(orgId.value, member) : null;
    });

    const profile = computed<ContactProfile | null>(() => (props.rootId ? profileOf(props.spaceKey, props.rootId) : null));
    const allTags = computed(() => contactsOf(props.spaceKey).tags);
    const isOrgAdmin = computed(() => !isPersonal.value && isAdmin(orgId.value));

    // 动作收口与通讯录资料卡同一份（删除朋友后的收尾=关抽屉）
    const { saveProfile, setBlocked, sendMessage, createTag, personalGroupOptions } = useContactActions({
      spaceKey: spaceKeyRef,
      contact,
      onDeleted: () => setVisible(false)
    });

    // 删除：个人=删除朋友；组织=管理员移出成员（use-delete-contact 双分支；
    // selectedRootId 的「删除后清空选中」收尾对抽屉无意义，传一次性占位 ref，
    // 关抽屉由下方 watch(contact) 统一负责）
    const { onDeleteContact } = useDeleteContact({
      spaceKey: spaceKeyRef,
      isPersonal,
      currentSpaceOrgId: orgId,
      selectedRootId: ref(''),
      selectedContact: contact,
      refreshOrganizations: async () => {
        try {
          await refreshOrganizations();
        } catch {
          // 刷新失败保留旧缓存（与 org-membership 错误策略一致）
        }
      }
    });

    // 展示期间联系人被删除（朋友被删/成员被移出刷新后）→ 收起抽屉
    watch(contact, (value) => {
      if (!value) {
        setVisible(false);
      }
    });

    /** 组织成员 -> 个人联系人：向个人空间发一条待确认请求（与 use-contact-panel.onAddAsFriend 同口径） */
    const onAddAsFriend = () => {
      const target = contact.value;
      if (!target) {
        return;
      }
      sendFriendRequest('personal', {
        rootId: target.rootId,
        raw: target.rootId,
        source: '组织成员',
        message: ''
      });
      ElMessage.success('已发送添加请求，等待对方确认（§9.3）');
    };

    const groupOptions = computed<GroupOption[]>(() =>
      isPersonal.value ? personalGroupOptions.value : orgGroupOptions(contactsOf(props.spaceKey).groupTree)
    );

    return {
      contact,
      profile,
      allTags,
      groupOptions,
      spaceType,
      isOrgAdmin,
      setVisible,
      saveProfile,
      setBlocked,
      sendMessage,
      createTag,
      onDeleteContact,
      onAddAsFriend
    };
  }
});
</script>
