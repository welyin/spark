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

/** 组织身份头像：某组织内「我」的身份（rail 组织空间头像 / 组织身份编辑页） */
export function orgIdentityAvatarSource(orgId: string): AvatarSource {
  const identity = getOrgIdentity(orgId);
  return {
    seed: `${currentUser.rootId ?? ''}@${orgId}`,
    // TODO(mock): 组织身份缺省占位名，待后端组织身份接口（ui-space-navbar §9.2）
    name: identity.nickname.trim() || '成员',
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
