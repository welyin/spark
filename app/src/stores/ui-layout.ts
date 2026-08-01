/**
 * 界面布局断点（移动端适配波次 1）：宽度 ≤768px 判定为移动端布局——
 * 左侧 rail 换成底部 tab 导航（MobileTabBar），设置/测试等次要入口收进顶栏「⋯」菜单。
 * 与 app-shell.css 中 @media (max-width: 768px) 共用同一断点；
 * matchMedia change 监听保证 dev 下拖动窗口跨越断点时两档布局实时切换。
 * 桌面端（≥769px）isMobileLayout 恒为 false，现有 rail + 顶栏布局不受影响。
 */
import { ref } from 'vue';

const media = window.matchMedia('(max-width: 768px)');

/** 是否移动端（窄屏）布局：true=底部 tab 导航，false=桌面 rail 导航 */
export const isMobileLayout = ref(media.matches);

media.addEventListener('change', (event) => {
  isMobileLayout.value = event.matches;
});

/**
 * 底部 tab 定义：激活态与 rail 共用 App.vue 的 activeTab（同一状态源）；
 * 「我的」直达 MinePage（桌面端为 rail 头像的隐藏入口，移动端提升为一级 tab）。
 * 设置/测试不在底部 tab，挪至顶栏右上角「⋯」菜单。
 */
export const MOBILE_TABS = [
  { id: 'messages', label: '消息' },
  { id: 'contacts', label: '通讯录' },
  { id: 'apps', label: '应用' },
  { id: 'mine', label: '我的' }
] as const;

export type MobileTabId = (typeof MOBILE_TABS)[number]['id'];
