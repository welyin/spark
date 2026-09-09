/**
 * 通讯录「删除朋友/成员」动作（自 ContactsPage 拆出以控制单文件行数）。
 * 个人空间：删除朋友为本地 mock（§5.5「删除同时自动拉黑」选项待真实模型落地），
 * 实现收口在 use-contact-actions.deleteFriend（这里只补「删除后清空选中」的收尾）；
 * 组织空间：管理员真实调用 organization.removeMember（§3.2/§5.5）。
 */
import type { ComputedRef, Ref } from 'vue';
import { ElMessage } from 'element-plus';
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
    // v1 缺口（A19 遗留）：壳层此处真实调用 organization.removeMember（§3.2/§5.5）；
    // organization 域管理面未进 SDK（接口面变更待拍板），插件版拦截并引导回旧 UI
    ElMessage.warning('插件版通讯录暂不支持移出组织成员，请切换旧内置界面操作（设置 → 通用 → 通讯录界面）');
  };

  return { onDeleteContact };
}
