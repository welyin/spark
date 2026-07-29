/**
 * 当前空间（模块级单例 ref）——设计文档 ui-space-navbar §10.1。
 *
 * 空间是前端概念：个人空间即「未选中任何组织」的 UI 状态，无后端实体。
 * 切换时持久化到 localStorage，下次启动恢复；恢复的组织已不存在时
 * 由 validateCurrentSpace 懒校验回退到个人空间。
 */
import { computed, ref } from 'vue';
import { findOrg, refreshOrganizations } from './org-membership';

export type CurrentSpace = { type: 'personal' } | { type: 'org'; orgId: string };

const STORAGE_KEY = 'spark:current-space';

/** 启动时从 localStorage 恢复；数据损坏/缺失一律回退个人空间。 */
function restoreSpace(): CurrentSpace {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<{ type: string; orgId: string }>;
      if (parsed.type === 'org' && typeof parsed.orgId === 'string' && parsed.orgId) {
        return { type: 'org', orgId: parsed.orgId };
      }
    }
  } catch {
    // 本地存储不可读（隐私模式等）时按默认处理
  }
  return { type: 'personal' };
}

export const currentSpace = ref<CurrentSpace>(restoreSpace());

export const currentSpaceType = computed<'personal' | 'org'>(() => currentSpace.value.type);

/** 组织空间时为 orgId，个人空间时为空串 */
export const currentSpaceOrgId = computed<string>(() =>
  currentSpace.value.type === 'org' ? currentSpace.value.orgId : ''
);

export function switchSpace(space: CurrentSpace): void {
  currentSpace.value = space;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(space));
  } catch {
    // 持久化失败不阻断切换（重启后回退个人空间）
  }
}

export function switchToPersonal(): void {
  switchSpace({ type: 'personal' });
}

export function switchToOrg(orgId: string): void {
  switchSpace({ type: 'org', orgId });
}

/**
 * 懒校验：启动恢复的组织可能已被删除/退出，组织列表（org-membership 缓存）
 * 中没有该组织时回退个人空间。接口失败（网络未就绪等）时保留现状，下一轮再校验。
 */
export async function validateCurrentSpace(): Promise<void> {
  if (currentSpace.value.type !== 'org') {
    return;
  }
  const orgId = currentSpace.value.orgId;
  try {
    await refreshOrganizations();
    if (!findOrg(orgId)) {
      switchToPersonal();
    }
  } catch {
    // 校验失败不清空用户状态
  }
}
