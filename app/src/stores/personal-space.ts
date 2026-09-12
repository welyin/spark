/**
 * 个人空间资料（本机偏好）：名字与 logo。
 *
 * 走查决策：个人空间允许用户设置名字和 logo（入口＝顶栏「当前空间」菜单的「空间设置」，
 * 组织空间同位为 OrgSettingsPanel）。数据只存本机（localStorage），不同步、不跨端——
 * 与桌面壁纸/排列方式同口径（组织只给默认，成员本地各自排布）。
 *
 * 所有展示「个人空间」名与 logo 的位置（rail 空间列表、顶栏、手机空间列表、
 * 桌面水印、全局搜索、窗口空间标签等）统一从这里取，不要各自写死「个人空间」。
 */
import { computed, reactive } from 'vue';

const STORAGE_KEY = 'spark:personal-space-profile';
/** 缺省名（未设置时的展示名） */
export const DEFAULT_PERSONAL_SPACE_NAME = '个人空间';

const profile = reactive({ name: '', logo: '' });

const load = () => {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      return;
    }
    const parsed = JSON.parse(raw) as { name?: unknown; logo?: unknown };
    if (typeof parsed.name === 'string') {
      profile.name = parsed.name;
    }
    if (typeof parsed.logo === 'string') {
      profile.logo = parsed.logo;
    }
  } catch {
    // 本地资料损坏：回退缺省
  }
};
load();

/** 个人空间展示名：未设置时为「个人空间」 */
export const personalSpaceName = computed(
  () => profile.name.trim() || DEFAULT_PERSONAL_SPACE_NAME,
);

/** 个人空间 logo（dataURL；空串＝用根身份头像作空间图标，保持原口径） */
export const personalSpaceLogo = computed(() => profile.logo);

/** 保存名字与 logo（name 为空串＝恢复缺省名；logo 为空串＝移除自定义 logo） */
export const savePersonalSpaceProfile = (name: string, logo: string) => {
  profile.name = name.trim();
  profile.logo = logo;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ name: profile.name, logo: profile.logo }));
  } catch {
    // 存储失败（如 logo 超配额）：内存态仍生效，刷新后回退
  }
};
