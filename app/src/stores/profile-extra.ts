/**
 * 资料扩展字段（性别/地区/签名）存取。
 *
 * 按作用域分两类存储，均已内核化：
 * - 个人空间键（rootId，不含 @）：读 = 从 rootIdentity.status 水合进
 *   本地缓存；写 = 调 rootIdentity.updateProfile（空串 = 清除，缺省 = 不变）。
 * - org 作用域键（rootId@orgId，与头像配色种子同键）：F2b 起走内核——
 *   读 = 从 organization.listMine 的成员条目水合；写 = organization.updateMyIdentity
 *   （空串 = 清除，缺省 = 不变；失败回滚 + console.warn，同个人键模式）。
 * 旧 localStorage（spark:profile-extra）org 键数据开发期直接丢弃，以内核为准。
 * 对外 API（getProfileExtra/setProfileExtra 签名）不变，组件零改动。
 */
import { ref, toRaw } from 'vue';
import type { OrgView } from '../api';

export type ProfileExtra = {
  /** '' 表示未设置 */
  gender: '' | '男' | '女';
  /** 地级市，如「杭州」；'' 表示未设置 */
  region: string;
  signature: string;
};

const DEFAULT_EXTRA: ProfileExtra = { gender: '', region: '', signature: '' };

/** org 作用域键（rootId@orgId）判定；个人空间键（纯 rootId）走 rootIdentity */
function isOrgScoped(key: string): boolean {
  return key.includes('@');
}

/** 内核值 → 本地模型：非法性别归一为 ''，null 归一为 '' */
function normalizeExtra(value: { gender?: string | null; region?: string | null; signature?: string | null }): ProfileExtra {
  return {
    gender: value.gender === '男' || value.gender === '女' ? value.gender : '',
    region: typeof value.region === 'string' ? value.region : '',
    signature: typeof value.signature === 'string' ? value.signature : ''
  };
}

/** 响应式映射：ProfileModule / OrgIdentityModule 表单直接依赖，写入后同步刷新 */
export const profileExtras = ref<Record<string, ProfileExtra>>({});

/** 已触发过内核水合的键（每键一次，避免读路径调用风暴） */
const hydratedKeys = new Set<string>();
/** 本地已写入的键：水合结果不得覆盖更新鲜的本地写（个人空间键用） */
const dirtyKeys = new Set<string>();
/** org 键写版本号：每次本地写 +1；org 水合/写回包只应用自己启动时的版本，
 * 避免「先开始的水合」覆盖「后完成的写」 */
const orgWriteVersions = new Map<string, number>();
/** 进行中的 listMine Promise：org 键并发水合共享同一次 IPC */
let orgListInflight: Promise<OrgView[]> | null = null;

function orgVersionOf(key: string): number {
  return orgWriteVersions.get(key) ?? 0;
}

/** 个人空间键的内核水合：rootIdentity.status → 缓存（组件经 ref 响应式刷新） */
async function hydrateFromKernel(rootId: string): Promise<void> {
  // 非 Tauri 环境（vitest/旧 Electron）无宿主 API：按默认空值展示；
  // 删除水合标记（与 org 键路径/org-identity/org-avatars 同口径），API 就绪后允许重新水合
  if (typeof window === 'undefined' || !window.electronAPI?.rootIdentity?.status) {
    hydratedKeys.delete(rootId);
    return;
  }
  try {
    const status = await window.electronAPI.rootIdentity.status();
    if (!status) {
      return;
    }
    if (status.rootId !== rootId) {
      // 非活动身份：取消标记，切到该身份后允许重新触发水合
      hydratedKeys.delete(rootId);
      return;
    }
    // 本地已有更新鲜的写时不覆盖
    if (dirtyKeys.has(rootId)) {
      return;
    }
    profileExtras.value = {
      ...profileExtras.value,
      [rootId]: normalizeExtra(status)
    };
  } catch {
    // 内核不可达（非 Tauri 环境等）按默认空值展示；
    // 取消标记允许下次读重试（与 org 键路径/org-identity/org-avatars 同口径）
    hydratedKeys.delete(rootId);
  }
}

/** org 作用域键的内核水合：listMine → 成员条目身份字段 → 缓存 */
async function hydrateOrgKeyFromKernel(key: string): Promise<void> {
  // 非 Tauri 环境（vitest/旧 Electron）无宿主 API：按默认空值展示；
  // 删除水合标记（与 org-identity/org-avatars 同口径），API 就绪后允许重新水合
  if (typeof window === 'undefined' || !window.electronAPI?.organization?.listMine) {
    hydratedKeys.delete(key);
    return;
  }
  const atIndex = key.indexOf('@');
  const rootId = key.slice(0, atIndex);
  const orgId = key.slice(atIndex + 1);
  const version = orgVersionOf(key);
  try {
    orgListInflight ??= window.electronAPI.organization.listMine().finally(() => {
      orgListInflight = null;
    });
    const list = await orgListInflight;
    const org = list.find((item) => item.orgId === orgId);
    const member = org?.members.find((item) => item.rootId === rootId);
    if (!member) {
      return;
    }
    // 水合期间有更新鲜的本地写时不覆盖
    if (orgVersionOf(key) !== version) {
      return;
    }
    profileExtras.value = {
      ...profileExtras.value,
      [key]: normalizeExtra(member)
    };
  } catch {
    // 内核不可达：取消标记，允许下次读重试
    hydratedKeys.delete(key);
  }
}

export function getProfileExtra(rootId: string): ProfileExtra {
  // 首次读触发内核水合（异步回填 ref，不改本函数同步签名）
  if (rootId && !hydratedKeys.has(rootId)) {
    hydratedKeys.add(rootId);
    if (isOrgScoped(rootId)) {
      void hydrateOrgKeyFromKernel(rootId);
    } else {
      void hydrateFromKernel(rootId);
    }
  }
  return profileExtras.value[rootId] ?? { ...DEFAULT_EXTRA };
}

export function setProfileExtra(rootId: string, patch: Partial<ProfileExtra>): void {
  if (!rootId) {
    return;
  }
  const previous = getProfileExtra(rootId);
  const written = { ...previous, ...patch };
  profileExtras.value = {
    ...profileExtras.value,
    [rootId]: written
  };
  // 组装内核补丁：空串 = 清除，缺省字段不传 = 不变
  const kernelPatch: { gender?: string; region?: string; signature?: string } = {};
  if (patch.gender !== undefined) {
    kernelPatch.gender = patch.gender;
  }
  if (patch.region !== undefined) {
    kernelPatch.region = patch.region;
  }
  if (patch.signature !== undefined) {
    kernelPatch.signature = patch.signature;
  }
  const rollback = (err: unknown): void => {
    // 写失败（锁定/非成员/IO 错误）：回滚展示值并解除脏标记，让后续水合用内核真值校正
    // （toRaw：ref 读出的是响应式代理，需比原始引用）
    console.warn('profile-extra 内核写入失败，已回滚展示值', err);
    if (toRaw(profileExtras.value[rootId]) === written) {
      profileExtras.value = {
        ...profileExtras.value,
        [rootId]: previous
      };
      dirtyKeys.delete(rootId);
    }
    // 删除水合标记：否则回滚到默认值后内核真值永远不来，下次读触发重新水合
    hydratedKeys.delete(rootId);
  };
  if (isOrgScoped(rootId)) {
    // org 作用域：写内核成员身份（F2b）；非 Tauri 环境保留本地值降级
    const version = orgVersionOf(rootId) + 1;
    orgWriteVersions.set(rootId, version);
    if (typeof window === 'undefined' || !window.electronAPI?.organization?.updateMyIdentity) {
      return;
    }
    const atIndex = rootId.indexOf('@');
    const selfRootId = rootId.slice(0, atIndex);
    const orgId = rootId.slice(atIndex + 1);
    void window.electronAPI.organization.updateMyIdentity(orgId, kernelPatch)
      .then((view) => {
        // 成功：用内核返回的视图校正缓存（对齐 org-identity 的 .then 校正模式），
        // 仅在无更新鲜本地写时应用
        if (orgVersionOf(rootId) !== version) {
          return;
        }
        const member = view.members.find((item) => item.rootId === selfRootId);
        if (member) {
          profileExtras.value = {
            ...profileExtras.value,
            [rootId]: normalizeExtra(member)
          };
        }
      })
      .catch((err: unknown) => {
        // 写失败：回滚展示值（仅无更新鲜本地写时），并删除水合标记——
        // 否则回滚到默认值后内核真值永远不来，下次读触发重新水合校正
        console.warn('profile-extra 内核写入失败，已回滚展示值', err);
        if (orgVersionOf(rootId) === version) {
          profileExtras.value = {
            ...profileExtras.value,
            [rootId]: previous
          };
        }
        hydratedKeys.delete(rootId);
      });
    return;
  }
  // 个人空间：写内核（缓存已先更新）；
  // 非 Tauri 环境（window.electronAPI 可能 undefined）保留本地值降级，不打脏标记
  if (typeof window === 'undefined' || !window.electronAPI?.rootIdentity?.updateProfile) {
    return;
  }
  dirtyKeys.add(rootId);
  void window.electronAPI.rootIdentity.updateProfile(kernelPatch).catch(rollback);
}
