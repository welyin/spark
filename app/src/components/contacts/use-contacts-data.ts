/**
 * 通讯录数据装载与联系人视图合成（自 ContactsPage 拆出以控制单文件行数）。
 *
 * 数据装载：组织空间成员为真实数据（organization.listMine，经 stores/org-membership
 * 模块级缓存共享）；联系人合成：个人=mock 朋友；组织=真实成员 + 本地附加资料（mock）。
 */
import { computed, onMounted, type ComputedRef, type Ref } from 'vue';
import { ElMessage } from 'element-plus';
import { organizations, refreshOrganizations as refreshOrgMembership } from '../../stores/org-membership';
import { profileOf, type SpaceContacts } from '../../mock/contacts';
import { friendContactItem, orgMemberContactItem } from './contact-item';
import type { ContactItem } from './types';

export interface ContactsDataContext {
  isPersonal: ComputedRef<boolean>;
  isOrg: ComputedRef<boolean>;
  spaceKey: ComputedRef<string>;
  spaceData: ComputedRef<SpaceContacts>;
  currentSpaceOrgId: ComputedRef<string>;
  keyword: Ref<string>;
}

export function useContactsData(ctx: ContactsDataContext) {
  const currentOrg = computed(() =>
    organizations.value.find((org) => org.orgId === ctx.currentSpaceOrgId.value) ?? null
  );
  const isOrgAdmin = computed(() => Boolean(currentOrg.value?.isCurrentUserAdmin));

  const refreshOrganizations = async () => {
    if (!ctx.isOrg.value) {
      return;
    }
    try {
      await refreshOrgMembership();
    } catch (error) {
      ElMessage.error(`加载组织成员失败：${error}`);
    }
  };

  onMounted(() => {
    void refreshOrganizations();
  });

  const contacts = computed<ContactItem[]>(() => {
    if (ctx.isPersonal.value) {
      // 拉黑的朋友不再出现在通讯录（黑名单管理在个人设置「朋友权限」）
      return ctx.spaceData.value.friends.filter((friend) => !friend.blocked).map(friendContactItem);
    }
    return (currentOrg.value?.members ?? [])
      .filter((member) => !profileOf(ctx.spaceKey.value, member.rootId).blocked)
      .map((member) => orgMemberContactItem(ctx.currentSpaceOrgId.value, member));
  });

  // TODO(mock): §2.4 要求按拼音搜索，当前首字母映射表不含全拼，暂只匹配名字/备注/标签/RootID
  const filteredContacts = computed<ContactItem[]>(() => {
    const kw = ctx.keyword.value.trim().toLowerCase();
    if (!kw) {
      return contacts.value;
    }
    const tagById = new Map(ctx.spaceData.value.tags.map((tag) => [tag.id, tag.name]));
    return contacts.value.filter((contact) => {
      const profile = profileOf(ctx.spaceKey.value, contact.rootId);
      const tagNames = profile.tagIds.map((id) => tagById.get(id) ?? '').join(' ');
      return [contact.displayName, profile.remark, contact.subtitle, contact.rootId, tagNames]
        .join('\n')
        .toLowerCase()
        .includes(kw);
    });
  });

  /** 搜索态：第二栏分组列表替换为扁平结果列表 */
  const searching = computed(() => Boolean(ctx.keyword.value.trim()));

  return { isOrgAdmin, contacts, filteredContacts, searching, refreshOrganizations };
}
