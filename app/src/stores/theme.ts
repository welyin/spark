/**
 * 主题模式（跟随系统 / 浅色 / 深色）：Element Plus 官方 dark/css-vars 方案，
 * 深色时给 <html> 挂 .dark 类。应用内 --spark-* 颜色令牌已别名到 --el-* 变量（tokens.css），
 * 组件库与自有界面一并随动；选择持久化在 localStorage。
 * localStorage 读写一律 try/catch：隐私模式下访问即抛，不能让主题模块加载失败拖垮应用（§4.4）。
 */
import { ref, watch } from 'vue';

export type ThemeMode = 'system' | 'light' | 'dark';

const STORAGE_KEY = 'spark:theme-mode';
const MODES: ThemeMode[] = ['system', 'light', 'dark'];

const readStoredMode = (): ThemeMode => {
  try {
    const stored = localStorage.getItem(STORAGE_KEY) as ThemeMode | null;
    return stored && MODES.includes(stored) ? stored : 'system';
  } catch {
    return 'system';
  }
};

export const themeMode = ref<ThemeMode>(readStoredMode());

const media = window.matchMedia('(prefers-color-scheme: dark)');

const apply = () => {
  const dark = themeMode.value === 'dark' || (themeMode.value === 'system' && media.matches);
  document.documentElement.classList.toggle('dark', dark);
};

/** 应用入口调用一次：立即生效 + 监听偏好变化与系统深浅色切换 */
export const initTheme = () => {
  apply();
  watch(themeMode, (mode) => {
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
      // 持久化失败不影响当次切换
    }
    apply();
  });
  media.addEventListener('change', apply);
};
