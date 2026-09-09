<!-- 手机端空间容器（space tab 两级结构的宿主，MobilePageTransition 内）：
     栈帧 root=域列表（SpaceListPage），push 'desktop' 帧=某域手机桌面（SpaceDesktop 图标网格）。
     出处 shell-mobile §3：第一级域列表 → 第二级该域手机桌面；点图标 open-app 上报
     open-plugin-tab（App.vue 渲染 PluginIframeHost 全屏 App，1.3 承接沉浸式）。
     桌面端（≥769px）不渲染本组件（PC 桌面另见阶段 2）。 -->
<template>
  <section class="space-mobile">
    <SpaceListPage v-if="frame.page === 'root'" />
    <SpaceDesktop
      v-else
      @back="backToList"
      @open-app="(payload) => emit('open-app', payload)"
      @open-market="emit('open-market')"
    />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import SpaceListPage from './SpaceListPage.vue';
import SpaceDesktop from './SpaceDesktop.vue';
import { currentPage, popPage } from '../stores/mobile-nav';
import type { OpenPluginTabPayload } from '../pages/AppsPage.vue';

const TAB = 'space';

export default defineComponent({
  name: 'SpaceMobile',
  components: { SpaceListPage, SpaceDesktop },
  emits: ['open-app', 'open-market'],
  setup(_, { emit }) {
    const frame = computed(() => currentPage(TAB));
    const backToList = () => popPage(TAB);

    return { frame, backToList, emit };
  }
});
</script>

<style scoped>
.space-mobile {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--spark-bg-page);
}
</style>
