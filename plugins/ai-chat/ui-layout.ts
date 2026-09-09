/**
 * 布局（插件内自包含）：壳层 isMobileLayout 是全局响应式布局信号；
 * 插件 iframe 内按自身视口宽度判定（窗口内容宽 = iframe 视口宽）。
 *
 * ai-chat 断点 600：侧栏固定 240px + 聊天区最小可用宽 ~320px，
 * 两栏并存需 ≥560px；默认窗宽 480（manifest window.defaultWidth）已低于
 * 该阈值，故 ≤600 一律窄窗（侧栏折叠为覆盖抽屉，顶栏 ☰ 打开），
 * >600 保持现有两栏布局。
 */
import { computed, ref, type Ref } from 'vue';

const width = ref(typeof window !== 'undefined' ? window.innerWidth : 1024);

if (typeof window !== 'undefined') {
  window.addEventListener('resize', () => {
    width.value = window.innerWidth;
  });
}

/** 窄窗布局（≤600px）：侧栏折叠为覆盖抽屉，聊天区全宽 */
export const isNarrowLayout: Ref<boolean> = computed(() => width.value <= 600);
