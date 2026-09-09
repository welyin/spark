/**
 * 资料扩展字段（性别/地区/签名）——插件版：壳层 profile-extra 依赖内核
 * identity 域扩展字段读口（未进 SDK 面，A19 遗留）。插件可得面：
 * sdk.runtime.currentRoot 携带当前身份扩展字段——故仅「自己」的资料
 * 扩展字段真实（入口握手后随 current-user 水合），他人/组织作用域键
 * v1 恒默认空（签名/性别不显示，与壳层缺省语义一致）。
 */
import { ref } from 'vue';

export type ProfileExtra = {
  /** '' 表示未设置 */
  gender: '' | '男' | '女';
  /** 地级市，如「杭州」；'' 表示未设置 */
  region: string;
  signature: string;
};

const DEFAULT_EXTRA: ProfileExtra = { gender: '', region: '', signature: '' };

/** 扩展字段缓存（键：rootId 个人 / rootId@orgId 组织作用域） */
export const profileExtras = ref<Record<string, ProfileExtra>>({});

/** 内核 gender 线形（'male'/'female'/null）→ 展示文案 */
function toGenderText(gender: string | null | undefined): ProfileExtra['gender'] {
  return gender === 'male' ? '男' : gender === 'female' ? '女' : '';
}

/** 写入当前身份扩展字段（index.ts 握手后随 currentRoot 水合调用） */
export function setSelfProfileExtra(rootId: string, extra: { gender?: string | null; region?: string | null; signature?: string | null }): void {
  profileExtras.value = {
    ...profileExtras.value,
    [rootId]: {
      gender: toGenderText(extra.gender),
      region: extra.region ?? '',
      signature: extra.signature ?? ''
    }
  };
}

/** 读扩展字段（未命中恒默认空；v1 仅当前身份经 setSelfProfileExtra 有真实值） */
export function getProfileExtra(rootId: string): ProfileExtra {
  return profileExtras.value[rootId] ?? { ...DEFAULT_EXTRA };
}
