/**
 * 组织身份（组织内昵称/头像 + 「使用个人身份」开关）存取——内核权威版。
 *
 * 权威来源：内核 `OrganizationMember` 的身份字段（nickname/avatar/
 * usePersonalIdentity），经 `organization.listMine`/`updateMyIdentity` 读写——
 * 组织记录走 pdsync（自设备）与 org 快照（成员间）双通道同步，跨设备生效。
 *
 * localStorage 保留为**种子**：启动/未水合时先展示上次的值，避免闪烁；
 * `refreshOrgIdentity` 以返回值为准覆盖缓存。
 *
 * 与 profile-extra 的关系：组织身份扩展字段（gender/region/signature）走
 * profile-extra（`rootId@orgId` 键），本模块只管昵称/头像/usePersonalIdentity。
 */
import { ref } from 'vue';
import { currentUser } from './current-user';
import { findOrg, organizations, refreshOrganizations } from './org-membership';

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

/** 仅更新本地缓存（水合/乐观更新用），不调内核 */
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

/**
 * 从内核组织列表水合指定组织（或全部）的本机成员身份字段。
 * 内核为准覆盖缓存；失败/未找到成员条目时保留现有缓存。
 */
export function refreshOrgIdentity(orgId?: string): void {
  void refreshOrganizations()
    .then(() => {
      const rootId = currentUser.rootId;
      if (!rootId) {
        return;
      }
      // 指定 orgId 只水合该组织；否则水合全部已加载组织
      const targets = orgId ? [orgId] : organizations.value.map((org) => org.orgId);
      let changed = false;
      for (const id of targets) {
        const me = findOrg(id)?.members.find((m) => m.rootId === rootId);
        if (!me) {
          continue;
        }
        orgIdentities.value = {
          ...orgIdentities.value,
          [id]: {
            nickname: me.nickname ?? '',
            avatar: me.avatar ?? '',
            usePersonalIdentity: me.usePersonalIdentity === true
          }
        };
        changed = true;
      }
      if (changed) {
        persist();
      }
    })
    .catch(() => {
      // 拉取失败保留现有缓存
    });
}

/** 写内核 + 乐观更新缓存。nickname 空串=清除；usePersonalIdentity 布尔直传 */
export function updateOrgIdentity(orgId: string, patch: Partial<OrgIdentity>): void {
  if (!orgId || typeof window === 'undefined' || !window.electronAPI?.organization?.updateMyIdentity) {
    return;
  }
  const kernelPatch: { nickname?: string; avatar?: string; usePersonalIdentity?: boolean } = {};
  if (patch.nickname !== undefined) {
    kernelPatch.nickname = patch.nickname;
  }
  if (patch.avatar !== undefined) {
    kernelPatch.avatar = patch.avatar;
  }
  if (patch.usePersonalIdentity !== undefined) {
    kernelPatch.usePersonalIdentity = patch.usePersonalIdentity;
  }
  if (Object.keys(kernelPatch).length === 0) {
    return;
  }
  void window.electronAPI.organization
    .updateMyIdentity(orgId, kernelPatch)
    .then(() => {
      // 内核成功：以返回的组织记录回写（成员字段为权威）
      setOrgIdentity(orgId, patch);
    })
    .catch(() => {
      // 失败不更新（与 profile-extra 写路径同口径）
    });
}
