/**
 * 联系人资料卡动作单点收口（个人空间）：备注保存 / 拉黑 / 删除朋友 / 发消息 / 新建标签 /
 * 个人空间分组下拉选项。通讯录资料卡（use-contact-panel / use-delete-contact 个人分支）
 * 与新朋友面板「已接受」内嵌资料卡（NewFriendsPanel）共用，不再各自维护一份。
 * 删除朋友的文案与确认框沿用原 use-delete-contact 个人分支；
 * 删除后的收尾差异（通讯录侧需清空选中、新朋友面板侧不需要）由可选 onDeleted 回调表达。
 */
import { computed, type ComputedRef } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import {
  contactsOf,
  createTag as mockCreateTag,
  removeFriend,
  setBlocked as mockSetBlocked,
  updateProfile,
  type ContactProfile,
  type ContactTag
} from '../../mock/contacts';
import { openChat } from './open-intents';
import type { ContactItem, GroupOption } from './types';

export interface ContactActionsContext {
  spaceKey: ComputedRef<string>;
  /** 目标联系人（资料卡当前展示的那位；null 时各动作空转） */
  contact: ComputedRef<ContactItem | null>;
  /** 删除朋友后的收尾（如清空选中）；缺省不做 */
  onDeleted?: () => void;
}

export function useContactActions(ctx: ContactActionsContext) {
  const saveProfile = (patch: Partial<ContactProfile>) => {
    if (ctx.contact.value) {
      updateProfile(ctx.spaceKey.value, ctx.contact.value.rootId, patch);
    }
  };

  const setBlocked = (blocked: boolean) => {
    if (!ctx.contact.value) {
      return;
    }
    mockSetBlocked(ctx.spaceKey.value, ctx.contact.value.rootId, blocked);
    ElMessage.success(blocked ? '已加入黑名单' : '已移出黑名单');
  };

  /** 删除朋友（仅个人空间调用方使用；组织空间的移出成员仍在 use-delete-contact） */
  const deleteFriend = async () => {
    const contact = ctx.contact.value;
    if (!contact || contact.isSelf) {
      return;
    }
    // TODO(mock): 删除朋友为本地 mock；§5.5「删除同时自动拉黑（可选）」的选项待真实模型落地
    try {
      await ElMessageBox.confirm(`确认删除朋友「${contact.displayName}」？`, '删除朋友', {
        type: 'warning',
        confirmButtonText: '删除',
        cancelButtonText: '取消'
      });
    } catch {
      return;
    }
    removeFriend(ctx.spaceKey.value, contact.rootId);
    ctx.onDeleted?.();
    ElMessage.success('朋友已删除');
  };

  /** 发送消息：打开/创建 1:1 会话（§5.3） */
  const sendMessage = () => {
    const contact = ctx.contact.value;
    if (!contact) {
      return;
    }
    openChat({ rootId: contact.rootId, name: contact.displayName });
  };

  const createTag = (name: string): ContactTag => mockCreateTag(ctx.spaceKey.value, name);

  /** 资料卡「分组」下拉（个人空间扁平，'' = 未分组；与 use-contact-groups 个人分支同口径） */
  const personalGroupOptions = computed<GroupOption[]>(() => [
    { id: '', label: '未分组' },
    ...contactsOf(ctx.spaceKey.value).groups.map((group) => ({ id: group.id, label: group.name }))
  ]);

  return { saveProfile, setBlocked, deleteFriend, sendMessage, createTag, personalGroupOptions };
}
