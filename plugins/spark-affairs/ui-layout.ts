/**
 * 布局信号（插件内自包含）：窗口宽度形态（PC 插件窗口化，ui-architecture §4.2；
 * WindowFrame 最小夹取 320×220，要求插件响应式）。
 *
 * 插件跑在 iframe 内，window.innerWidth 即窗口内容区宽度（移动全屏时为屏宽），
 * 故本模块只暴露宽度信号、不做 (pointer:coarse) 形态判定——本插件无「退出插件」
 * 类壳元素需要按形态显隐（非沉浸式，移动全屏由壳层顶栏提供返回）。
 *
 * 用途：
 * - 议题详情抽屉尺寸：窄窗（<600）全宽 100%——固定 70% 在 320 窗口下只剩 224px，
 *   时间线/表格不可用；宽窗 70%——保留左侧议题墙上下文；
 * - 详情元信息 el-descriptions 列数：窄窗单列堆叠（双栏在 320–480 下单格不足
 *   150px，标签挤压换行不可读）。
 */
import { computed, ref } from 'vue';

/** 窄窗阈值（px）：600×0.7=420 起详情双栏元信息尚可读，之下抽屉全宽 + 单列 */
const NARROW_MAX_WIDTH = 600;

const viewportWidth = ref(typeof window === 'undefined' ? NARROW_MAX_WIDTH : window.innerWidth);

if (typeof window !== 'undefined') {
  window.addEventListener('resize', () => {
    viewportWidth.value = window.innerWidth;
  });
}

/** 窄窗形态（<600px）：详情抽屉全宽、元信息单列堆叠 */
export const isNarrowLayout = computed(() => viewportWidth.value < NARROW_MAX_WIDTH);

/** 议题详情抽屉尺寸（el-drawer size）：窄窗全宽，宽窗 70% */
export const detailDrawerSize = computed(() => (isNarrowLayout.value ? '100%' : '70%'));
