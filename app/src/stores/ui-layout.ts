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
 * M4/M9 走查定稿（docs/ui/problem.md）：五一级入口 消息 · 事务 · 空间 · 应用 · 设置——
 * 「应用」＝系统层应用管理（AppsPage 升格一级 Tab）；第 5 位「设置」＝设置列表页
 * （个人设置｜系统设置｜测试｜退出登录，原「我的」内容并入个人设置，MinePage 不再作独立 Tab）。
 * 通讯录退出一级（转空间插件，docs/ui §八决策 1）。
 */
export const MOBILE_TABS = [
  { id: 'messages', label: '消息' },
  { id: 'affairs', label: '事务' },
  { id: 'space', label: '空间' },
  { id: 'apps', label: '应用' },
  { id: 'settings', label: '设置' }
] as const;

export type MobileTabId = (typeof MOBILE_TABS)[number]['id'];
