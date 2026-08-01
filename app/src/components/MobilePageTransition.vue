<!-- 移动端栈帧转场容器（移动端适配波次 3）：以 tab + page + params 序列化为 :key 包裹当前栈帧层，
     按 mobile-nav 记录的最近栈动作选方向——push 新层自右滑入（旧层轻微左移），pop/reset 反向滑出，
     微信式减速曲线（样式见 app-shell.css 波次 3 媒体查询块）。仅各页面的移动端渲染分支使用；
     桌面端（isMobileLayout=false）不渲染本组件，无任何动画。 -->
<template>
  <Transition :name="transitionName">
    <div :key="frameKey" class="mobile-stack-stage">
      <slot />
    </div>
  </Transition>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { currentPage, lastNavAction } from '../stores/mobile-nav';

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

    return { frameKey, transitionName };
  }
});
</script>
