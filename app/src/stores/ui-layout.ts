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
 * 底部 tab 定义：激活态与 rail 共用 App.vue 的 activeTab（同一状态源）。
 * 新 UI 四一级入口（docs/ui/README §五）：消息 · 空间 · 事务 · 我的，「我的」固定最右第四位。
 * 阶段 0：space / affairs 为占位页（建设中），正式形态按 todo 阶段 1/3 补齐；
 * 通讯录退出一级（转空间插件），应用市场收进空间（docs/ui §八决策 1/2）。
 * 设置/测试不在底部 tab，挪至顶栏右上角「⋯」菜单。
 */
export const MOBILE_TABS = [
  { id: 'messages', label: '消息' },
  { id: 'space', label: '空间' },
  { id: 'affairs', label: '事务' },
  { id: 'mine', label: '我的' }
] as const;

export type MobileTabId = (typeof MOBILE_TABS)[number]['id'];
