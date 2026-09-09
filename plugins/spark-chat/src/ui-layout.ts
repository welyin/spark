/**
 * 布局（插件内自包含）：壳层 isMobileLayout 是全局响应式布局信号；
 * 插件 iframe 内按自身视口宽度判定（≤768px 为移动布局）。
 */
import { computed, ref, type Ref } from 'vue';

const width = ref(typeof window !== 'undefined' ? window.innerWidth : 1024);

if (typeof window !== 'undefined') {
  window.addEventListener('resize', () => {
    width.value = window.innerWidth;
  });
}

/** 移动布局（≤768px） */
export const isMobileLayout: Ref<boolean> = computed(() => width.value <= 768);
