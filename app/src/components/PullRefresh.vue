<!-- 微博式下拉刷新容器（M8，docs/ui/problem.md）：触摸下拉触发刷新，带阻尼跟随、
     松手回弹（transition）与加载指示；消息页/事务页共用，不引第三方库。
     触发条件：手势起点到本容器根之间的所有祖先元素 scrollTop 均为 0（列表滚到顶部），
     且下拉位移过阈值（64px）后松手才触发 onRefresh；未过阈值回弹复位。
     内部滚动冲突处理：上滑（dy<=0）或任一祖先已滚动时不接管手势、不 preventDefault，
     列表原生滚动不受影响。 -->
<template>
  <div
    ref="rootEl"
    class="pull-refresh"
    @touchstart="onTouchstart"
    @touchmove="onTouchmove"
    @touchend="onTouchend"
    @touchcancel="onTouchend"
  >
    <!-- 指示条：高度随手势增长（flex 布局自然把内容顶下去），松手后 transition 回弹 -->
    <div class="pull-refresh-indicator" :style="indicatorStyle">
      <template v-if="distance > 0">
        <el-icon v-if="refreshing" :size="14" class="is-loading"><Loading /></el-icon>
        <span class="pull-refresh-text">{{ indicatorText }}</span>
      </template>
    </div>
    <div class="pull-refresh-content">
      <slot />
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { Loading } from '@element-plus/icons-vue';

/** 触发刷新的下拉阈值（px，阻尼前手势位移经 0.5 阻尼后的距离） */
const THRESHOLD = 64;
/** 下拉最大位移（px） */
const MAX_DISTANCE = 120;
/** 阻尼系数：指示条位移 = 手指位移 × 0.5（微博式减速手感） */
const DAMPING = 0.5;
/** 刷新中指示条停留高度 */
const REFRESHING_HOLD = 40;

/** 手势起点到容器根之间是否有已滚动的元素（有则不接管，让列表原生滚动） */
const hasScrolledAncestor = (target: EventTarget | null, boundary: HTMLElement | null): boolean => {
  let node = target instanceof HTMLElement ? target : null;
  while (node && node !== boundary) {
    if (node.scrollTop > 0) {
      return true;
    }
    node = node.parentElement;
  }
  return false;
};

const touchY = (event: TouchEvent): number => event.touches[0]?.clientY ?? 0;

export default defineComponent({
  name: 'PullRefresh',
  components: { Loading },
  props: {
    /** 刷新动作：返回 Promise 时指示条保持到其完成 */
    onRefresh: { type: Function as PropType<() => void | Promise<void>>, required: true }
  },
  setup(props) {
    const rootEl = ref<HTMLElement | null>(null);
    /** 指示条当前高度（=下拉位移） */
    const distance = ref(0);
    const refreshing = ref(false);
    /** 手指接触中（跟手阶段无 transition；松手后开启 transition 回弹） */
    const touching = ref(false);
    /** 手势起点 Y（null=未在跟踪手势） */
    let startY: number | null = null;

    const onTouchstart = (event: TouchEvent) => {
      if (refreshing.value || event.touches.length !== 1) {
        return;
      }
      // 仅列表滚到顶部时才可能接管：手势起点向上任一祖先已滚动则不跟踪
      if (hasScrolledAncestor(event.target, rootEl.value)) {
        return;
      }
      startY = touchY(event);
      touching.value = true;
    };

    const onTouchmove = (event: TouchEvent) => {
      if (startY === null || refreshing.value) {
        return;
      }
      const dy = touchY(event) - startY;
      if (dy <= 0) {
        // 上滑/回拖：不接管（列表原生滚动）；已拉出的指示条收回
        distance.value = 0;
        return;
      }
      // 下拉：接管手势并阻止默认滚动/越界回弹
      if (event.cancelable) {
        event.preventDefault();
      }
      distance.value = Math.min(MAX_DISTANCE, dy * DAMPING);
    };

    const onTouchend = () => {
      if (startY === null) {
        return;
      }
      startY = null;
      touching.value = false;
      if (distance.value >= THRESHOLD && !refreshing.value) {
        refreshing.value = true;
        distance.value = REFRESHING_HOLD;
        Promise.resolve()
          .then(() => props.onRefresh())
          .catch(() => {})
          .finally(() => {
            refreshing.value = false;
            distance.value = 0;
          });
      } else {
        distance.value = 0;
      }
    };

    const indicatorStyle = computed(() => ({
      height: `${distance.value}px`,
      transition: touching.value ? 'none' : 'height 200ms ease'
    }));

    const indicatorText = computed(() => {
      if (refreshing.value) {
        return '正在刷新…';
      }
      return distance.value >= THRESHOLD ? '释放立即刷新' : '下拉刷新';
    });

    return { rootEl, distance, refreshing, indicatorStyle, indicatorText, onTouchstart, onTouchmove, onTouchend };
  }
});
</script>

<style scoped>
.pull-refresh {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  /* 抑制浏览器/Android WebView 原生下拉越界效果，避免与手势指示条双重反馈 */
  overscroll-behavior-y: contain;
}

.pull-refresh-indicator {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  overflow: hidden;
  color: var(--spark-text-3);
  font-size: var(--spark-font-size-secondary);
}

.pull-refresh-content {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overscroll-behavior-y: contain;
}
</style>
