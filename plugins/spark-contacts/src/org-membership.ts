/**
 * 我加入的组织列表（插件版，与壳层 stores/org-membership 同语义）：
 * 模块级 ref + 并发去重（同一时间只有一次真实调用，并发调用方共享同一
 * Promise）；数据源 sdk.runtime.listMineOrganizations（`org:read` 基础权限，
 * 与壳层 organization.listMine 等语义）。
 *
 * 错误策略：refresh 失败时保留旧缓存并把错误抛给调用方。
 */
import { ref } from 'vue';
import type { OrgView } from './host-types';
import { listMineOrganizations } from './sdk-host';
import { setOrgAvatar } from './org-avatars';

/** 我加入的组织列表缓存；只允许由 refreshOrganizations 写入 */
export const organizations = ref<OrgView[]>([]);

/** 进行中的拉取 Promise：并发 refresh 共享同一次调用 */
let inflight: Promise<OrgView[]> | null = null;

/** 拉取并刷新组织列表缓存。并发调用去重；失败时缓存不变，错误抛给调用方。 */
export function refreshOrganizations(): Promise<OrgView[]> {
  if (!inflight) {
    inflight = listMineOrganizations()
      .then((list) => {
        organizations.value = list;
        // 内核 OrgView.avatar（组织 logo）写入本地 org-avatars 展示缓存
        // （v1 桩 no-op；内核为空时保留本地已有值的语义随桩一并简化）
        for (const org of list) {
          if (org.avatar) {
            setOrgAvatar(org.orgId, org.avatar);
          }
        }
        return list;
      })
      .finally(() => {
        inflight = null;
      });
  }
  return inflight;
}

/** 按 orgId 查缓存中的组织（未加载/不存在时为 null） */
export function findOrg(orgId: string): OrgView | null {
  return organizations.value.find((org) => org.orgId === orgId) ?? null;
}

/** 组织名（未加载/不存在时为 null，调用方自行回退默认文案） */
export function nameOf(orgId: string): string | null {
  return findOrg(orgId)?.name ?? null;
}

/**
 * 是否组织管理员：缺省判断当前用户（OrgView.isCurrentUserAdmin）；
 * 传 rootId 时按成员角色判断。组织未加载/不存在时为 false。
 */
export function isAdmin(orgId: string, rootId?: string): boolean {
  const org = findOrg(orgId);
  if (!org) {
    return false;
  }
  if (rootId === undefined) {
    return org.isCurrentUserAdmin;
  }
  return org.members.some((member) => member.rootId === rootId && member.role === 'admin');
}
