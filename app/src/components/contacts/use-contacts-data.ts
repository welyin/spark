/**
 * 通讯录数据装载与联系人视图合成（自 ContactsPage 拆出以控制单文件行数）。
 *
 * 数据装载：组织空间成员为真实数据（organization.listMine，经 stores/org-membership
 * 模块级缓存共享）；联系人合成：个人=mock 朋友；组织=真实成员 + 本地附加资料（mock）。
 */
import { computed, onMounted, ref, type ComputedRef, type Ref } from 'vue';
import { ElMessage } from 'element-plus';
import { currentUser } from '../../stores/current-user';
import { getOrgIdentity } from '../../stores/org-identity';
import { getProfileExtra } from '../../stores/profile-extra';
import { organizations, refreshOrganizations as refreshOrgMembership } from '../../stores/org-membership';
import { memberIdentityOf, profileOf, type SpaceContacts } from '../../mock/contacts';
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
  const currentRootId = ref('');

  const currentOrg = computed(() =>
    organizations.value.find((org) => org.orgId === ctx.currentSpaceOrgId.value) ?? null
  );
  const isOrgAdmin = computed(() => Boolean(currentOrg.value?.isCurrentUserAdmin));

  const loadCurrentRootId = async () => {
    try {
      const status = await window.electronAPI.rootIdentity.status();
      currentRootId.value = status.rootId ?? '';
    } catch {
      currentRootId.value = '';
    }
  };

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
    void loadCurrentRootId();
    void refreshOrganizations();
  });

  const contacts = computed<ContactItem[]>(() => {
    if (ctx.isPersonal.value) {
      // 拉黑的朋友不再出现在通讯录（黑名单管理在个人设置「朋友权限」）
      return ctx.spaceData.value.friends.filter((friend) => !friend.blocked).map((friend) => {
        const isSelf = friend.rootId === currentRootId.value;
        // 自己：性别/签名/头像取本地真实资料（内核朋友记录的 gender/signature 目前不落库，
        // 网络同步也还没有档案通道）；其他朋友用内核数据（gender 缺省则不显示图标，与组织同组件同规则）
        const extra = isSelf ? getProfileExtra(friend.rootId) : null;
        const signature = extra ? extra.signature : friend.signature;
        const gender = extra
          ? extra.gender === '男'
            ? ('male' as const)
            : extra.gender === '女'
              ? ('female' as const)
              : undefined
          : friend.gender;
        return {
          rootId: friend.rootId,
          displayName: friend.remark || friend.nickname,
          // 第二行展示签名（无签名则不显示），RootID 属隐私不上列表
          subtitle: signature,
          avatarImage: isSelf ? currentUser.avatar : '',
          signature,
          gender,
          nickname: friend.nickname,
          blocked: friend.blocked,
          // 内核默认把自己作为联系人（发消息给自己=同步到所有个人节点）
          isSelf
        };
      });
    }
    return (currentOrg.value?.members ?? [])
      .filter((member) => !profileOf(ctx.spaceKey.value, member.rootId).blocked)
      .map((member) => {
        const profile = profileOf(ctx.spaceKey.value, member.rootId);
        const isSelf = member.rootId === currentRootId.value;
        // 组织身份头像配色种子 rootId@orgId，与个人身份区分
        const seed = `${member.rootId}@${ctx.currentSpaceOrgId.value}`;
        // 自己：用真实组织身份（组织身份模块编辑的那份 + 扩展字段），不吃成员假数据
        if (isSelf) {
          const orgIdentity = getOrgIdentity(ctx.currentSpaceOrgId.value);
          const extra = getProfileExtra(seed);
          const nickname = orgIdentity.nickname.trim() || '成员';
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
        // 其他成员：组织身份为确定性 mock（待内核组织身份接口）
        const identity = memberIdentityOf(ctx.spaceKey.value, member.rootId);
        return {
          rootId: member.rootId,
          displayName: profile.remark || identity.nickname,
          subtitle: identity.signature,
          avatarSeed: seed,
          signature: identity.signature,
          gender: identity.gender,
          nickname: identity.nickname,
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
