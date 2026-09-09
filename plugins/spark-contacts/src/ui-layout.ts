/**
 * 布局（插件内自包含）：壳层 isMobileLayout 是全局响应式布局信号；
 * 插件 iframe 内按自身视口宽度判定（<840px 为移动布局）。
 *
 * 断点高于壳层全局的 768：桌面形态为三/四栏（左栏 280 + 成员列 min-width 280 +
 * 资料列 min-width 280 = 最小 840px），PC 窗口形态下 769–839px 宽度带会横向溢出，
 * 故桌面分栏只在视口 ≥840px 时启用（contacts.css 的 @media 与本断点同值）。
 */
import { computed, ref, type Ref } from 'vue';

/** 移动/桌面布局断点（px）：桌面三栏最小宽 840（280×3） */
const DESKTOP_MIN_WIDTH = 840;

const width = ref(typeof window !== 'undefined' ? window.innerWidth : 1024);

if (typeof window !== 'undefined') {
  window.addEventListener('resize', () => {
    width.value = window.innerWidth;
  });
}

/** 移动布局（<840px：整页 + 插件内导航栈） */
export const isMobileLayout: Ref<boolean> = computed(() => width.value < DESKTOP_MIN_WIDTH);
