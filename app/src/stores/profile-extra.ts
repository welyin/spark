/**
 * 资料扩展字段（性别/地区/签名）本地存取。
 *
 * 内核 rootIdentity.updateProfile 目前仅支持 nickname/avatar，
 * 本期前端方案：按身份键存 localStorage——个人资料用 rootId，
 * 组织身份用 rootId@orgId（与头像配色种子同键），待内核资料字段扩展后迁移。
 */
import { ref } from 'vue';

// TODO(mock): 性别/地区/签名仅存 localStorage，待内核 updateProfile 支持扩展字段后改为真实接口
const STORAGE_KEY = 'spark:profile-extra';

export type ProfileExtra = {
  /** '' 表示未设置 */
  gender: '' | '男' | '女';
  /** 地级市，如「杭州」；'' 表示未设置 */
  region: string;
  signature: string;
};

const DEFAULT_EXTRA: ProfileExtra = { gender: '', region: '', signature: '' };

function load(): Record<string, ProfileExtra> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, Partial<ProfileExtra>>;
      const result: Record<string, ProfileExtra> = {};
      for (const [rootId, value] of Object.entries(parsed)) {
        result[rootId] = {
          gender: value.gender === '男' || value.gender === '女' ? value.gender : '',
          region: typeof value.region === 'string' ? value.region : '',
          signature: typeof value.signature === 'string' ? value.signature : ''
        };
      }
      return result;
    }
  } catch {
    // 本地存储不可读时按空表处理
  }
  return {};
}

/** 响应式映射：ProfileModule / OrgIdentityModule 表单直接依赖，写入后同步刷新 */
export const profileExtras = ref<Record<string, ProfileExtra>>(load());

function persist(): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(profileExtras.value));
  } catch {
    // 持久化失败不阻断展示
  }
}

export function getProfileExtra(rootId: string): ProfileExtra {
  return profileExtras.value[rootId] ?? { ...DEFAULT_EXTRA };
}

export function setProfileExtra(rootId: string, patch: Partial<ProfileExtra>): void {
  if (!rootId) {
    return;
  }
  profileExtras.value = {
    ...profileExtras.value,
    [rootId]: { ...getProfileExtra(rootId), ...patch }
  };
  persist();
}
