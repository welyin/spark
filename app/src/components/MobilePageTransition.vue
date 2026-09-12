<!-- 移动端栈帧转场容器（移动端适配波次 3）：以 tab + page + params 序列化为 :key 包裹当前栈帧层，
     按 mobile-nav 记录的最近栈动作选方向——push 新层自右滑入（旧层轻微左移），pop/reset 反向滑出，
     微信式减速曲线（样式见 app-shell.css 波次 3 媒体查询块）。仅各页面的移动端渲染分支使用；
     桌面端（isMobileLayout=false）不渲染本组件，无任何动画。
     M14：屏幕左缘右滑返回——手势起点 clientX ≤ 24px 才接管，水平位移 ≥ 48px 且明显水平
     （|dx| > 1.5×|dy|）松手时触发返回：覆盖层（设置内容页等）优先逐层关闭，否则 popPage 回上一帧。
     返回层级链：二级页 → 一级列表（域桌面 → 域列表同此）；全屏 App（插件 tab）不在 mobile-nav
     栈内，其左缘返回需 App.vue 壳层接线（见 problem.md M14 交付注记）。
     不做跟手平移（仅触发式手势），真机手感走查后再决定是否加随动。 -->
<template>
  <Transition :name="transitionName">
    <div
      :key="frameKey"
      class="mobile-stack-stage"
      @touchstart="onTouchstart"
      @touchmove="onTouchmove"
      @touchend="onTouchend"
      @touchcancel="onTouchend"
    >
      <slot />
    </div>
  </Transition>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { canBack, currentPage, lastNavAction, popPage } from '../stores/mobile-nav';
import { hasOverlay, requestCloseOverlay } from '../stores/overlay-stack';

/** 左缘热区宽度（px）：手势起点在此内才接管，避免与页内横向滚动冲突 */
const EDGE_WIDTH = 24;
/** 触发返回的最小水平位移（px） */
const TRIGGER_DX = 48;

export default defineComponent({
  name: 'MobilePageTransition',
  props: {
    /** 本页在导航栈中的 tab 键（与 App.vue activeTab 一致） */
    tab: { type: String, required: true }
  },
  setup(props) {
    /** 栈帧键：tab+page+params 序列化——同页不同参（如 chat A→chat B）也触发 push 转场 */
    const frameKey = computed(() => {
      const frame = currentPage(props.tab);
      return `${props.tab}:${frame.page}:${JSON.stringify(frame.params ?? {})}`;
    });

    /** 转场方向：push=右滑入；pop/reset（回栈底/清帧）=反向滑出；无记录或非本 tab 动作时不动画 */
    const transitionName = computed(() => {
      const action = lastNavAction.value;
      if (!action || action.tab !== props.tab) {
        return '';
      }
      return action.type === 'push' ? 'mobile-nav-push' : 'mobile-nav-pop';
    });

    // ---- M14 左缘右滑返回 ----
    let startX: number | null = null;
    let startY = 0;
    let dx = 0;
    let dy = 0;

    const onTouchstart = (event: TouchEvent) => {
      if (event.touches.length !== 1) {
        return;
      }
      const touch = event.touches[0];
      // 栈深 1（一级页）且无覆盖层时不跟踪：一级页的「返回」语义是切 tab/退出，不归本手势
      if (!canBack(props.tab) && !hasOverlay()) {
        return;
      }
      if (touch.clientX > EDGE_WIDTH) {
        return;
      }
      startX = touch.clientX;
      startY = touch.clientY;
      dx = 0;
      dy = 0;
    };

    const onTouchmove = (event: TouchEvent) => {
      if (startX === null) {
        return;
      }
      const touch = event.touches[0];
      if (!touch) {
        return;
      }
      dx = touch.clientX - startX;
      dy = touch.clientY - startY;
    };

    const onTouchend = () => {
      if (startX === null) {
        return;
      }
      startX = null;
      // 明显水平的右滑才触发返回（斜滑/竖滑让位给列表滚动）
      if (dx < TRIGGER_DX || dx <= Math.abs(dy) * 1.5) {
        return;
      }
      // 与 Android 系统返回键同口径：覆盖层优先逐层关闭，再退导航栈帧
      if (hasOverlay()) {
        requestCloseOverlay();
        return;
      }
      popPage(props.tab);
    };

    return { frameKey, transitionName, onTouchstart, onTouchmove, onTouchend };
  }
});
</script>
