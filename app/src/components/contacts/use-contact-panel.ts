/**
 * 通讯录第四栏资料卡与申请处理（自 ContactsPage 拆出以控制单文件行数）：
 * 选中联系人/资料卡/标签成员抽屉、资料卡动作（备注/拉黑/删除/发消息/加朋友）、
 * 朋友请求与申请处理（§9.3）、全局搜索「打开联系人资料」请求的消费。
 */
import { computed, onMounted, ref, watch, type ComputedRef, type Ref } from 'vue';
import { ElMessage } from 'element-plus';
import { organizations } from '../../stores/org-membership';
import { consumePendingContact, pendingContact } from '../../stores/pending-contact';
import {
  profileOf,
  replyOutgoing,
  resolveRequest,
  retryOutgoing,
  sendFriendRequest,
  type ContactProfile,
  type FriendPermission
} from '../../mock/contacts';
import { CONTACT_INTENT_ADD, CONTACT_INTENT_BROWSE } from './open-intents';
import { useContactActions } from './use-contact-actions';
import { useDeleteContact } from './use-delete-contact';
import type { ContactItem, GroupOption, RightView } from './types';

export interface ContactPanelContext {
  isPersonal: ComputedRef<boolean>;
  isOrg: ComputedRef<boolean>;
  isOrgAdmin: ComputedRef<boolean>;
  spaceKey: ComputedRef<string>;
  currentSpaceOrgId: ComputedRef<string>;
  contacts: ComputedRef<ContactItem[]>;
  searching: ComputedRef<boolean>;
  groupOptions: ComputedRef<GroupOption[]>;
  selectedRootId: Ref<string>;
  rightView: Ref<RightView>;
  activeGroupId: Ref<string>;
  addFriendVisible: Ref<boolean>;
  inviteVisible: Ref<boolean>;
  refreshOrganizations: () => Promise<void>;
}

export function useContactPanel(ctx: ContactPanelContext) {
  const selectedContact = computed(() =>
    ctx.contacts.value.find((contact) => contact.rootId === ctx.selectedRootId.value) ?? null
  );
  const selectedProfile = computed<ContactProfile | null>(() =>
    ctx.selectedRootId.value ? profileOf(ctx.spaceKey.value, ctx.selectedRootId.value) : null
  );

  /** 标签成员详情抽屉 */
  const drawerVisible = ref(false);

  /** 标签页成员行点击：选中该联系人并在抽屉中打开资料卡 */
  const onViewMember = (rootId: string) => {
    if (!ctx.contacts.value.some((contact) => contact.rootId === rootId)) {
      return;
    }
    ctx.selectedRootId.value = rootId;
    drawerVisible.value = true;
  };

  // 联系人被删除（或切空间导致选中失效）后收起抽屉
  watch(selectedContact, (contact) => {
    if (!contact) {
      drawerVisible.value = false;
    }
  });

  /** 把第三栏切到某联系人所在分组（分组 id 无效时落到「未分组」） */
  const syncActiveGroup = (rootId: string) => {
    const groupId = profileOf(ctx.spaceKey.value, rootId).groupId;
    ctx.activeGroupId.value =
      groupId && ctx.groupOptions.value.some((option) => option.id === groupId) ? groupId : 'ungrouped';
  };

  /** 选中联系人：右栏回到详情态；搜索态跳转时第三栏同步到其所在分组 */
  const openPanel = (contact: ContactItem) => {
    ctx.selectedRootId.value = contact.rootId;
    ctx.rightView.value = 'contact';
    if (ctx.searching.value) {
      syncActiveGroup(contact.rootId);
    }
  };

  /** 打开添加对话框：个人空间=添加朋友；组织空间=邀请成员（仅管理员） */
  const openAddDialog = () => {
    if (ctx.isPersonal.value) {
      ctx.addFriendVisible.value = true;
    } else if (ctx.isOrgAdmin.value) {
      ctx.inviteVisible.value = true;
    } else {
      ElMessage.warning('只有组织管理员可以添加成员');
    }
  };

  // 消费「打开联系人资料」请求（全局搜索跳转，见 stores/pending-contact）：
  // 仅当联系人在当前空间列表中时选中；组织成员为异步加载，watch contacts 待数据就绪后再消费。
  // 消息页空状态跳转走哨兵值（components/contacts/open-intents）：仅落地 / 打开添加对话框
  const openPendingContact = () => {
    const target = pendingContact.value;
    if (!target) {
      return;
    }
    if (target.rootId === CONTACT_INTENT_BROWSE) {
      consumePendingContact();
      return;
    }
    if (target.rootId === CONTACT_INTENT_ADD) {
      // 组织空间需等组织数据就绪以判断管理员身份（watcher 会在数据到达后再次触发）
      if (ctx.isOrg.value && organizations.value.length === 0) {
        return;
      }
      consumePendingContact();
      openAddDialog();
      return;
    }
    if (!ctx.contacts.value.some((contact) => contact.rootId === target.rootId)) {
      return;
    }
    consumePendingContact();
    ctx.selectedRootId.value = target.rootId;
    ctx.rightView.value = 'contact';
    syncActiveGroup(target.rootId);
  };
  onMounted(openPendingContact);
  watch([pendingContact, ctx.contacts], openPendingContact);

  // 资料卡动作（备注/拉黑/发消息/新建标签）收口在 use-contact-actions；删除走 use-delete-contact
  const contactActions = useContactActions({ spaceKey: ctx.spaceKey, contact: selectedContact });
  const onSaveProfile = contactActions.saveProfile;
  const onSetBlocked = contactActions.setBlocked;
  const onSendMessage = contactActions.sendMessage;
  const onCreateTagReturn = contactActions.createTag;

  const onDeleteContact = useDeleteContact({
    spaceKey: ctx.spaceKey,
    isPersonal: ctx.isPersonal,
    currentSpaceOrgId: ctx.currentSpaceOrgId,
    selectedRootId: ctx.selectedRootId,
    selectedContact,
    refreshOrganizations: ctx.refreshOrganizations
  }).onDeleteContact;

  // ------------------------------------------------------------------
  // 添加：朋友请求 / 申请处理 / 组织成员转个人联系人（§9.3）
  // ------------------------------------------------------------------

  const onAddFriendSubmit = (payload: {
    rootId: string;
    raw: string;
    peerId?: string;
    addresses?: string[];
    source: string;
    message: string;
  }) => {
    sendFriendRequest(ctx.spaceKey.value, payload);
    ElMessage.success('添加请求已发送，等待对方确认（双向确认 §4.1）');
  };

  /** 重新发送发出申请（投递失败重试 / 等待确认中重发——对方接受回执可能丢失） */
  const onRetryOutgoing = (requestId: string) => {
    retryOutgoing(ctx.spaceKey.value, requestId);
    ElMessage.success('已重新发送，等待对方确认');
  };

  /** 回复对方的询问（双方可继续互复，直到对方拒绝/接受） */
  const onReplyOutgoing = (requestId: string, text: string) => {
    replyOutgoing(ctx.spaceKey.value, requestId, text);
  };

  const onResolveRequest = (requestId: string, accept: boolean, permission: FriendPermission) => {
    // TODO(mock): 组织空间的「接受」仅本地改状态；真实流程走邀请码（§4.2）
    resolveRequest(ctx.spaceKey.value, requestId, accept, permission);
    if (accept) {
      ElMessage.success(ctx.isPersonal.value ? '已添加为朋友' : '已接受申请（真实加入走邀请码流程）');
    }
  };

  /** 组织成员 -> 个人联系人：向个人空间发一条待确认请求（mock，§9.3 双向确认） */
  const onAddAsFriend = () => {
    const contact = selectedContact.value;
    if (!contact) {
      return;
    }
    sendFriendRequest('personal', {
      rootId: contact.rootId,
      raw: contact.rootId,
      source: '组织成员',
      message: ''
    });
    ElMessage.success('已发送添加请求，等待对方确认（§9.3）');
  };

  // ------------------------------------------------------------------
  // 标签（§8.2）：新建/重命名/删除/成员编辑均在 TagManager 内直写 mock store；
  // 资料卡备注对话框里的「新建标签并选中」入口收口在 use-contact-actions.createTag
  // ------------------------------------------------------------------

  return {
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
    onAddAsFriend,
    onCreateTagReturn
  };
}
