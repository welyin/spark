/**
 * 头像取数统一入口——所有展示头像的位置从这里拿「种子 / 名称 / 图片」三件套，
 * 不再各自拼装。配色种子规则（UserAvatar 按种子哈希配色，同种子恒同色）：
 * - 个人身份：rootId
 * - 组织身份（某组织内「我」）：rootId@orgId（与个人身份配色区分开）
 * - 组织 logo：orgId（自定义 logo 优先，缺省自动配色）
 */
import { currentUser } from './current-user';
import { getOrgIdentity } from './org-identity';
import { orgAvatars } from './org-avatars';
import { friendOf, profileOf, spaceKeyOf } from '../mock/contacts';

export type AvatarSource = {
  /** 自动配色哈希种子（传 UserAvatar 的 root-id） */
  seed: string;
  /** 自动头像取首字 / alt 文案 */
  name: string;
  /** 已上传图片（dataURL）；空串 = 自动配色头像 */
  image: string;
};

/** 个人身份头像：当前登录用户（rail 头像 / 空间切换器 / 自己的消息气泡 / 个人设置页头等） */
export function personalAvatarSource(): AvatarSource {
  return {
    seed: currentUser.rootId ?? '',
    name: currentUser.nickname.trim() || '未命名用户',
    image: currentUser.avatar
  };
}

/** 组织身份头像：某组织内「我」的身份（rail 组织空间头像 / 组织身份编辑页）。
 *  昵称为空时回退个人昵称（个人昵称为空再走其「未命名用户」兜底）；
 *  其它字段（头像/签名/性别）不做回退，空就是空。 */
export function orgIdentityAvatarSource(orgId: string): AvatarSource {
  const identity = getOrgIdentity(orgId);
  return {
    seed: `${currentUser.rootId ?? ''}@${orgId}`,
    name: identity.nickname.trim() || personalAvatarSource().name,
    image: identity.avatar
  };
}

/** 组织 logo：自定义 logo 优先，缺省按 orgId 哈希自动配色（OrgAvatar 之外的场合用） */
export function orgLogoSource(orgId: string, name: string): AvatarSource {
  return {
    seed: orgId,
    name,
    image: orgAvatars.value[orgId] ?? ''
  };
}

/** 任意个人（按 rootId）的统一头像入口：自己→个人身份；朋友→朋友记录（网络同步来的
 *  头像实时生效，名称备注优先）；其余（未接受的申请人/陌生人）→ 调用方快照兜底，
 *  再退自动配色头像。图片与名称按「朋友记录 > 快照」分别归并（快照只补朋友记录缺的），
 *  各展示位（新朋友列表/聊天头/会话列表/消息气泡）一律从这里取，不再各自查好友记录。 */
export function personAvatarSource(
  spaceKey: string,
  rootId: string,
  fallback?: { name?: string; image?: string }
): AvatarSource {
  if (rootId && rootId === currentUser.rootId) {
    return personalAvatarSource();
  }
  const friend = friendOf(spaceKey, rootId);
  if (friend) {
    return {
      seed: rootId,
      name: friend.remark || friend.nickname || fallback?.name || '',
      image: friend.avatar || fallback?.image || ''
    };
  }
  return { seed: rootId, name: fallback?.name ?? '', image: fallback?.image ?? '' };
}

/** 个人展示名统一入口（备注 > 昵称 > 调用方兜底）：与 personAvatarSource 同一套归并逻辑。
 *  所有展示「这个人叫什么」的位置（联系人/会话列表/聊天窗/新朋友列表…）一律从这里取，
 *  改了备注名全网展示位同步生效，新增展示位也不要再各自拼 remark||nickname。 */
export function personDisplayName(spaceKey: string, rootId: string, fallback = ''): string {
  return personAvatarSource(spaceKey, rootId, { name: fallback }).name || fallback;
}

/** 组织成员展示名统一入口（组织空间本地备注 > 个人朋友展示名 > 调用方兜底）。
 *  成员的真实身份只可能来自个人空间朋友记录（含网络同步来的昵称）；非朋友成员
 *  没有身份来源，直接用调用方兜底（一般为截断 rootId）。所有展示「这个组织成员
 *  叫什么」的位置（通讯录/全局搜索/权限名单/网关设置…）一律从这里取，
 *  改了备注全网展示位同步生效，不要再各自拼 remark||nickname/shortRootId。 */
export function orgMemberDisplayName(orgId: string, rootId: string, fallback = ''): string {
  const spaceKey = spaceKeyOf({ type: 'org', orgId });
  const remark = profileOf(spaceKey, rootId).remark;
  if (remark) {
    return remark;
  }
  if (friendOf('personal', rootId)) {
    return personDisplayName('personal', rootId, fallback);
  }
  return fallback;
}

/** 组织成员（按 orgId+rootId）的统一头像入口：配色种子 rootId@orgId（与个人身份配色区分，
 *  与 use-contacts-data 组织分支同规则）；名称与 orgMemberDisplayName 同一套归并；
 *  图片优先个人朋友头像（朋友记录同步来的 dataURL），其次调用方兜底，空则自动配色头像。 */
export function orgMemberAvatarSource(
  orgId: string,
  rootId: string,
  fallback?: { name?: string; image?: string }
): AvatarSource {
  const friend = friendOf('personal', rootId);
  return {
    seed: `${rootId}@${orgId}`,
    name: orgMemberDisplayName(orgId, rootId, fallback?.name ?? ''),
    image: (friend ? personAvatarSource('personal', rootId, fallback).image : fallback?.image) ?? ''
  };
}
