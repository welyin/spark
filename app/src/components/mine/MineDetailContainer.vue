<!-- mine 模块详情容器：column 模式渲染原第四栏 .mine-detail div；drawer 模式渲染详情容器。
     各模块的详情/编辑内容作为 slot 传入，同一份内容多种容器复用。
     移动端（≤768px，Android 前端改造）：drawer 模式渲染为「顶部返回栏 + 内容」的整页，
     自右滑入 / 向右滑出（与导航栈 push/pop 呼应）；注册到覆盖层栈，系统回退键优先关闭本层。
     桌面端保持 el-drawer 侧拉抽屉。 -->
<template>
  <!-- 移动端：整页详情（返回栏 + 内容），微信式；仅选中字段（open）时显示 -->
  <Transition name="overlay-slide">
    <div
      v-if="drawer && isMobileLayout && open"
      class="mine-detail-page"
    >
      <MobileBackBar :title="title" @back="emit('close')" />
      <div class="mine-detail mine-detail-page-body">
        <slot />
      </div>
    </div>
  </Transition>

  <!-- 桌面端：侧拉抽屉（右上角 X 关闭） -->
  <el-drawer
    v-if="drawer && !isMobileLayout"
    :model-value="open"
    :with-header="false"
    size="420px"
    class="app-drawer"
    @update:model-value="onUpdate"
  >
    <button type="button" class="app-drawer-close" title="关闭" @click="emit('close')">
      <el-icon :size="16"><Close /></el-icon>
    </button>
    <!-- 复用 .mine-detail：卡片去边框/阴影（mine.css），内容铺满抽屉 -->
    <div class="mine-detail app-drawer-fill">
      <slot />
    </div>
  </el-drawer>

  <!-- column 模式：第四栏 -->
  <div v-if="!drawer" class="mine-detail">
    <slot />
  </div>
</template>

<script lang="ts">
import { defineComponent, onBeforeUnmount, onMounted, watch } from 'vue';
import { Close } from '@element-plus/icons-vue';
import MobileBackBar from '../MobileBackBar.vue';
import { isMobileLayout } from '../../stores/ui-layout';
import { isOverlayCloseTarget, popOverlay, pushOverlay } from '../../stores/overlay-stack';

export default defineComponent({
  name: 'MineDetailContainer',
  components: { Close, MobileBackBar },
  props: {
    /** true=抽屉模式（移动端=整页，桌面端=抽屉）；false=第四栏模式 */
    drawer: { type: Boolean, default: false },
    /** 抽屉/详情是否打开（通常绑定「是否有选中项」） */
    open: { type: Boolean, default: false },
    /** 详情标题（移动端整页的返回栏标题） */
    title: { type: String, default: '' }
  },
  emits: ['close'],
  setup(props, { emit }) {
    const onUpdate = (value: boolean) => {
      if (!value) {
        emit('close');
      }
    };

    // 覆盖层栈登记（token 制）：移动端整页详情打开时入栈持有 token，关闭/卸载时凭 token 出栈；
    // 系统回退键经 overlay-stack 仅关闭栈顶覆盖层（叠层时逐层回退，不会"跳两层"）
    const isMobileOverlay = () => props.drawer && isMobileLayout.value;
    let overlayToken: symbol | null = null;
    const releaseOverlay = () => {
      popOverlay(overlayToken);
      overlayToken = null;
    };
    watch(
      () => props.open && isMobileOverlay(),
      (opened) => {
        if (opened && !overlayToken) {
          overlayToken = pushOverlay();
        } else if (!opened) {
          releaseOverlay();
        }
      },
      { immediate: true }
    );
    onBeforeUnmount(releaseOverlay);

    // 系统回退键请求关闭覆盖层：仅当本层是栈顶时响应
    const onCloseOverlay = (event: Event) => {
      if (isOverlayCloseTarget(event, overlayToken) && props.open && isMobileOverlay()) {
        emit('close');
      }
    };
    onMounted(() => window.addEventListener('spark:close-overlay', onCloseOverlay));
    onBeforeUnmount(() => window.removeEventListener('spark:close-overlay', onCloseOverlay));

    return { onUpdate, emit, isMobileLayout };
  }
});
</script>

<style scoped>
/* 移动端整页详情：返回栏 + 内容占满；进出均有水平滑动动画（微信式 push/pop 呼应） */
.mine-detail-page {
  position: absolute;
  inset: 0;
  z-index: 3;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-card);
  box-shadow: -12px 0 24px rgba(0, 0, 0, 0.12);
}

.mine-detail-page-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}

/* 进出动画（与导航栈同款减速曲线与方向：进=右滑入，出=向右滑出） */
.overlay-slide-enter-active,
.overlay-slide-leave-active {
  transition: transform 260ms cubic-bezier(0.25, 0.46, 0.45, 0.94);
  will-change: transform;
}

.overlay-slide-enter-from {
  transform: translateX(100%);
}

.overlay-slide-leave-to {
  transform: translateX(100%);
}
</style>
