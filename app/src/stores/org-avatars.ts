/**
 * 组织 logo（dataURL）本地存取——所有展示组织头像的位置统一走这里。
 *
 * 内核 OrgView 暂无 avatar 字段，本期前端方案：按 orgId 存 localStorage。
 * 读取返回空串时由 OrgAvatar 按 orgId 哈希自动生成头像。
 */
import { ref } from 'vue';

// TODO(mock): 待 OrganizationRecord.avatar 后端字段落地后改为读写组织记录（ui-space-navbar §11.2）
const STORAGE_KEY = 'spark:org-avatars';

function load(): Record<string, string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      const result: Record<string, string> = {};
      for (const [orgId, value] of Object.entries(parsed)) {
        if (typeof value === 'string') {
          result[orgId] = value;
        }
      }
      return result;
    }
  } catch {
    // 本地存储不可读时按空表处理
  }
  return {};
}

/** 响应式映射：OrgAvatar/SpaceSwitcher 直接依赖，写入后各处同步刷新 */
export const orgAvatars = ref<Record<string, string>>(load());

function persist(): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(orgAvatars.value));
  } catch {
    // 持久化失败不阻断展示
  }
}

export function getOrgAvatar(orgId: string): string {
  return orgAvatars.value[orgId] ?? '';
}

export function setOrgAvatar(orgId: string, dataUrl: string): void {
  if (!orgId) {
    return;
  }
  if (dataUrl) {
    orgAvatars.value = { ...orgAvatars.value, [orgId]: dataUrl };
  } else {
    const next = { ...orgAvatars.value };
    delete next[orgId];
    orgAvatars.value = next;
  }
  persist();
}
