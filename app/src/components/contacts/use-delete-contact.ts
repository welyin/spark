/**
 * 通讯录「删除朋友/成员」动作（自 ContactsPage 拆出以控制单文件行数）。
 * 个人空间：删除朋友为本地 mock（§5.5「删除同时自动拉黑」选项待真实模型落地），
 * 实现收口在 use-contact-actions.deleteFriend（这里只补「删除后清空选中」的收尾）；
 * 组织空间：管理员真实调用 organization.removeMember（§3.2/§5.5）。
 */
import type { ComputedRef, Ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { useContactActions } from './use-contact-actions';
import type { ContactItem } from './types';

export interface DeleteContactContext {
  spaceKey: ComputedRef<string>;
  isPersonal: ComputedRef<boolean>;
  currentSpaceOrgId: ComputedRef<string>;
  selectedRootId: Ref<string>;
  selectedContact: ComputedRef<ContactItem | null>;
  refreshOrganizations: () => Promise<void>;
}

export function useDeleteContact(ctx: DeleteContactContext) {
  const { deleteFriend } = useContactActions({
    spaceKey: ctx.spaceKey,
    contact: ctx.selectedContact,
    // 通讯录侧删除后清空选中（新朋友面板侧无选中态，不传）
    onDeleted: () => {
      ctx.selectedRootId.value = '';
    }
  });

  const onDeleteContact = async () => {
    const contact = ctx.selectedContact.value;
    if (!contact || contact.isSelf) {
      return;
    }
    if (ctx.isPersonal.value) {
      await deleteFriend();
      return;
    }
    // 组织空间：管理员真实调用 removeMember（§3.2/§5.5）
    try {
      await ElMessageBox.confirm(`确认将成员「${contact.displayName}」移出组织？`, '删除成员', {
        type: 'warning',
        confirmButtonText: '确认移除',
        cancelButtonText: '取消'
      });
    } catch {
      return;
    }
    try {
      await window.electronAPI.organization.removeMember(ctx.currentSpaceOrgId.value, contact.rootId);
      ElMessage.success('成员已移除');
      ctx.selectedRootId.value = '';
      await ctx.refreshOrganizations();
    } catch (error) {
      ElMessage.error(`移除成员失败：${error}`);
    }
  };

  return { onDeleteContact };
}
