/**
 * 布局（插件内自包含）：壳层 isMobileLayout 是全局响应式布局信号；
 * 插件 iframe 内按自身视口宽度判定（窗口内容宽 = iframe 视口宽）。
 *
 * spark-minichat 断点 560（口径同 ai-chat 断点逻辑）：侧栏固定 240px +
 * 会话区最小可用宽 ~320px，两栏并存需 ≥560px；≤560 一律窄窗（侧栏折叠
 * 为覆盖抽屉，会话区全宽 + ☰ 打开入口），>560 保持两栏布局。
 */
import { computed, ref, type Ref } from 'vue';

const width = ref(typeof window !== 'undefined' ? window.innerWidth : 1024);

if (typeof window !== 'undefined') {
  window.addEventListener('resize', () => {
    width.value = window.innerWidth;
  });
}

/** 窄窗布局（≤560px）：侧栏折叠为覆盖抽屉，会话区全宽 */
export const isNarrowLayout: Ref<boolean> = computed(() => width.value <= 560);
