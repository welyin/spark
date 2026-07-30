/**
 * 通讯录数据装载与联系人视图合成（自 ContactsPage 拆出以控制单文件行数）。
 *
 * 数据装载：组织空间成员为真实数据（organization.listMine，经 stores/org-membership
 * 模块级缓存共享）；联系人合成：个人=mock 朋友；组织=真实成员 + 本地附加资料（mock）。
 */
import { computed, onMounted, type ComputedRef, type Ref } from 'vue';
import { ElMessage } from 'element-plus';
import { currentUser } from '../../stores/current-user';
import { getOrgIdentity } from '../../stores/org-identity';
import { getProfileExtra } from '../../stores/profile-extra';
import {
  orgMemberAvatarSource,
  orgMemberDisplayName,
  personAvatarSource,
  personDisplayName
} from '../../stores/avatar-sources';
import { organizations, refreshOrganizations as refreshOrgMembership } from '../../stores/org-membership';
import { friendOf, profileOf, type SpaceContacts } from '../../mock/contacts';
import { friendContactItem } from './contact-item';
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
      .map((member) => {
        const profile = profileOf(ctx.spaceKey.value, member.rootId);
        const isSelf = member.rootId === currentUser.rootId;
        if (isSelf) {
          const orgIdentity = getOrgIdentity(ctx.currentSpaceOrgId.value);
          // 「使用个人身份」开启：该组织内自己按个人身份展示（与 UserAvatarMenu 同口径）
          if (orgIdentity.usePersonalIdentity) {
            const extra = getProfileExtra(member.rootId);
            const name = personDisplayName('personal', member.rootId);
            return {
              rootId: member.rootId,
              displayName: profile.remark || name,
              subtitle: extra.signature,
              // 个人身份配色种子 rootId（与个人空间一致），不走 rootId@orgId
              avatarSeed: member.rootId,
              avatarImage: personAvatarSource('personal', member.rootId).image,
              signature: extra.signature,
              gender: extra.gender === '男' ? ('male' as const) : extra.gender === '女' ? ('female' as const) : undefined,
              nickname: name,
              blocked: profile.blocked,
              isSelf: true,
              role: member.role,
              joinedAt: member.joinedAt
            };
          }
          // 自己（组织身份）：用真实组织身份（组织身份模块编辑的那份 + 扩展字段）
          // 组织身份头像配色种子 rootId@orgId，与个人身份区分（统一入口 orgMemberAvatarSource）
          const seed = orgMemberAvatarSource(ctx.currentSpaceOrgId.value, member.rootId).seed;
          const extra = getProfileExtra(seed);
          // 昵称为空时回退个人昵称（与 orgIdentityAvatarSource 同口径）；其它字段不回退
          const nickname = orgIdentity.nickname.trim() || personDisplayName('personal', member.rootId);
          return {
            rootId: member.rootId,
            displayName: profile.remark || nickname,
            subtitle: extra.signature,
            avatarSeed: seed,
            avatarImage: orgIdentity.avatar,
            signature: extra.signature,
            gender: extra.gender === '男' ? ('male' as const) : extra.gender === '女' ? ('female' as const) : undefined,
            nickname,
            blocked: profile.blocked,
            isSelf: true,
            role: member.role,
            joinedAt: member.joinedAt
          };
        }
        // 其他成员：名称/头像走统一入口（个人朋友记录为唯一真实身份来源）；
        // 签名/性别只在成员是个人朋友时取朋友记录字段，否则不显示
        const source = orgMemberAvatarSource(ctx.currentSpaceOrgId.value, member.rootId);
        const friend = friendOf('personal', member.rootId);
        return {
          rootId: member.rootId,
          displayName: orgMemberDisplayName(ctx.currentSpaceOrgId.value, member.rootId),
          subtitle: friend?.signature ?? '',
          avatarSeed: source.seed,
          avatarImage: source.image,
          signature: friend?.signature ?? '',
          gender: friend?.gender,
          nickname: friend?.nickname,
          blocked: profile.blocked,
          isSelf: false,
          role: member.role,
          joinedAt: member.joinedAt
        };
      });
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
