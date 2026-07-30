/**
 * friend（个人空间朋友记录）→ ContactItem 的单点映射：
 * 通讯录联系人列表（use-contacts-data 个人分支）与新朋友面板「已接受」内嵌资料卡共用，
 * 不再各自维护一份（历史上两份已漂移：头像一个手拼一个走统一入口，
 * 「自己」判定一个自拉 rootIdentity.status() 一个用 currentUser.rootId）。
 *
 * 组织成员 → ContactItem（orgMemberContactItem）同样收口在此：
 * 通讯录联系人列表（use-contacts-data 组织分支）与聊天窗资料卡抽屉（ContactCardDrawer）
 * 共用「自己/他人/使用个人身份」三分支构造，呼应数据源统一约定。
 */
import { currentUser } from '../../stores/current-user';
import { getOrgIdentity } from '../../stores/org-identity';
import { getProfileExtra } from '../../stores/profile-extra';
import {
  orgMemberAvatarSource,
  orgMemberDisplayName,
  personAvatarSource,
  personDisplayName
} from '../../stores/avatar-sources';
import { friendOf, profileOf, spaceKeyOf, type MockFriend, type OrgGroupNode } from '../../mock/contacts';
import type { OrgView } from '../../api';
import type { ContactItem, GroupOption } from './types';

/** 本地资料的性别文案（'男'/'女'/''）→ ContactItem 的 'male'/'female'/undefined（与组织成员同组件同规则） */
const toGender = (gender: string): ContactItem['gender'] =>
  gender === '男' ? 'male' : gender === '女' ? 'female' : undefined;

export function friendContactItem(friend: MockFriend): ContactItem {
  // 「自己」判定统一用 currentUser 单例（App 挂载时已刷新），不再各自拉 rootIdentity.status()
  const isSelf = friend.rootId === currentUser.rootId;
  // 自己：性别/签名取本地真实资料（内核朋友记录的 gender/signature 目前不落库，
  // 网络同步也还没有档案通道）；其他朋友用内核数据（gender 缺省则不显示图标，与组织同组件同规则）
  const extra = isSelf ? getProfileExtra(friend.rootId) : null;
  const signature = extra ? extra.signature : friend.signature;
  return {
    rootId: friend.rootId,
    displayName: personDisplayName('personal', friend.rootId),
    // 第二行展示签名（无签名则不显示），RootID 属隐私不上列表
    subtitle: signature,
    // 统一入口：自己→个人身份头像，朋友→朋友记录头像（含同步更新）
    avatarImage: personAvatarSource('personal', friend.rootId).image,
    signature,
    gender: extra ? toGender(extra.gender) : friend.gender,
    nickname: friend.nickname,
    blocked: friend.blocked,
    // 内核默认把自己作为联系人（发消息给自己=同步到所有个人节点）
    isSelf
  };
}

type OrgMember = OrgView['members'][number];

/** 组织成员 → ContactItem：「自己（使用个人身份）/ 自己（组织身份）/ 他人」三分支的唯一构造入口 */
export function orgMemberContactItem(orgId: string, member: OrgMember): ContactItem {
  const profile = profileOf(spaceKeyOf({ type: 'org', orgId }), member.rootId);
  const isSelf = member.rootId === currentUser.rootId;
  if (isSelf) {
    const orgIdentity = getOrgIdentity(orgId);
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
        gender: toGender(extra.gender),
        nickname: name,
        blocked: profile.blocked,
        isSelf: true,
        role: member.role,
        joinedAt: member.joinedAt
      };
    }
    // 自己（组织身份）：用真实组织身份（组织身份模块编辑的那份 + 扩展字段）
    // 组织身份头像配色种子 rootId@orgId，与个人身份区分（统一入口 orgMemberAvatarSource）
    const seed = orgMemberAvatarSource(orgId, member.rootId).seed;
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
      gender: toGender(extra.gender),
      nickname,
      blocked: profile.blocked,
      isSelf: true,
      role: member.role,
      joinedAt: member.joinedAt
    };
  }
  // 其他成员：名称/头像走统一入口（个人朋友记录为唯一真实身份来源）；
  // 签名/性别只在成员是个人朋友时取朋友记录字段，否则不显示
  const source = orgMemberAvatarSource(orgId, member.rootId);
  const friend = friendOf('personal', member.rootId);
  return {
    rootId: member.rootId,
    displayName: orgMemberDisplayName(orgId, member.rootId),
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
}

/** 资料卡「分组」下拉的组织树扁平化选项（按深度缩进；'' = 未分组），与 use-contact-groups 同口径 */
export function orgGroupOptions(nodes: OrgGroupNode[]): GroupOption[] {
  const flatten = (list: OrgGroupNode[], depth = 0): GroupOption[] =>
    list.flatMap((node) => [
      { id: node.id, label: `${'　'.repeat(depth)}${node.name}` },
      ...flatten(node.children, depth + 1)
    ]);
  return [{ id: '', label: '未分组' }, ...flatten(nodes)];
}
