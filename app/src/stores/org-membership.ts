/**
 * 我加入的组织列表（模块级单例缓存）——收敛散落的 organization.listMine() 调用。
 *
 * 原来 9 处调用点各自拉取同一份列表；这里统一为模块级 ref + 并发去重：
 * 同一时间只有一次真实 IPC，并发调用方共享同一个 Promise。
 * 不做 localStorage 持久化（列表以内核为准，启动后由各调用方 refresh 填充）。
 *
 * 错误策略：refresh 失败时保留旧缓存并把错误抛给调用方，由各调用点按自己的
 * 现状处理（忽略 / 回退默认 / toast），与重构前行为一致。
 *
 * 注意：本模块不 import current-space（current-space 的 validateCurrentSpace
 * 依赖本模块，反向 import 会构成循环依赖）；需要当前空间 orgId 时由调用方传参。
 */
import { ref } from 'vue';
import type { OrgView } from '../api';
import { setOrgAvatar } from './org-avatars';

/** 我加入的组织列表缓存；只允许由 refreshOrganizations 写入 */
export const organizations = ref<OrgView[]>([]);

/** 进行中的拉取 Promise：并发 refresh 共享同一次 IPC */
let inflight: Promise<OrgView[]> | null = null;

/**
 * 拉取并刷新组织列表缓存。并发调用去重；失败时缓存不变，错误抛给调用方。
 */
export function refreshOrganizations(): Promise<OrgView[]> {
  if (!inflight) {
    inflight = window.electronAPI.organization
      .listMine()
      .then((list) => {
        organizations.value = list;
        // 内核 OrgView.avatar（组织 logo）写入本地 org-avatars 展示缓存，让其他成员
        // 也能看到组织 logo；内核为空时保留本地已有值，避免清掉存量本地 logo
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
