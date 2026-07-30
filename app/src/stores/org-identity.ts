/**
 * 组织身份（组织内昵称/头像 + 「使用个人身份」开关）本地存取。
 *
 * 内核无组织身份接口（OrganizationMember 无 nickname/avatar 字段），
 * 本期前端方案：按 orgId 存 localStorage；昵称为空时展示侧回退个人昵称
 * （见 avatar-sources.ts orgIdentityAvatarSource），其它字段为空即为空、不做兜底。
 */
import { ref } from 'vue';

// TODO(mock): 待 OrganizationMember.nickname/avatar 后端字段与更新接口落地后改为读写内核（ui-space-navbar §9.4）
const STORAGE_KEY = 'spark:org-identity';

export type OrgIdentity = {
  nickname: string;
  avatar: string;
  /** 开启后在该组织内所有场景使用个人头像/昵称替代组织身份 */
  usePersonalIdentity: boolean;
};

const DEFAULT_IDENTITY: OrgIdentity = { nickname: '', avatar: '', usePersonalIdentity: false };

function load(): Record<string, OrgIdentity> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, Partial<OrgIdentity>>;
      const result: Record<string, OrgIdentity> = {};
      for (const [orgId, value] of Object.entries(parsed)) {
        result[orgId] = {
          nickname: typeof value.nickname === 'string' ? value.nickname : '',
          avatar: typeof value.avatar === 'string' ? value.avatar : '',
          usePersonalIdentity: value.usePersonalIdentity === true
        };
      }
      return result;
    }
  } catch {
    // 本地存储不可读时按空表处理
  }
  return {};
}

/** 响应式映射：rail 头像（UserAvatarMenu）与 MinePage 组织身份模块（OrgIdentityModule）直接依赖，写入后同步刷新 */
export const orgIdentities = ref<Record<string, OrgIdentity>>(load());

function persist(): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(orgIdentities.value));
  } catch {
    // 持久化失败不阻断展示
  }
}

export function getOrgIdentity(orgId: string): OrgIdentity {
  return orgIdentities.value[orgId] ?? { ...DEFAULT_IDENTITY };
}

export function setOrgIdentity(orgId: string, patch: Partial<OrgIdentity>): void {
  if (!orgId) {
    return;
  }
  orgIdentities.value = {
    ...orgIdentities.value,
    [orgId]: { ...getOrgIdentity(orgId), ...patch }
  };
  persist();
}
