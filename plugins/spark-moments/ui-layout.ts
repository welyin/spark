/**
 * 布局（插件内自包含）：窗口形态信号（PC 插件窗口化，ui-architecture §4.2）。
 *
 * 时间线为单列流式布局、无宽度断点（max-width 640 居中自适应，320/480/880 三档
 * 同构），故本模块不暴露宽度信号，只回答一个问题：当前是否触屏移动形态（全屏 App）。
 *
 * 用途：插件内「退出朋友圈」按钮的显隐——
 * - PC 窗口模式：WindowFrame 自带关闭钮，插件内退出钮语义重复，隐藏；
 * - 移动全屏：壳层沉浸式（chrome.hostTitleBar:false）不提供可见返回（仅 Android
 *   硬件返回键可退出），插件内退出钮是唯一可见出口，保留。
 *
 * 判定用 pointer 媒体查询而非视口宽度：PC 窗口最小宽 320 与手机全屏宽度带重叠，
 * 宽度无法区分两种形态；coarse=触屏（移动全屏）、fine=精确指针（PC 窗口）。
 * 触屏 PC（二合一）会判为 coarse 而保留按钮——冗余万元素，不影响关窗。
 */
import { computed, ref, type Ref } from 'vue';

const TOUCH_QUERY = '(pointer: coarse)';
const canQuery = typeof window !== 'undefined' && typeof window.matchMedia === 'function';

const coarse = ref(canQuery ? window.matchMedia(TOUCH_QUERY).matches : false);

if (canQuery) {
  window.matchMedia(TOUCH_QUERY).addEventListener('change', (event) => {
    coarse.value = event.matches;
  });
}

/** 触屏移动形态（全屏 App）：保留插件内「退出朋友圈」按钮 */
export const isTouchLayout: Ref<boolean> = computed(() => coarse.value);
