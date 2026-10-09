<!-- 应用图标统一组件（插件版，plugin-dist §2.3 回退链的插件可自足子集）：
     ① 包内图标（plugin:// 源）在插件沙箱 iframe 内不可达（该协议只服务壳层
        系统域），本版不实现——已安装应用图标以声明图标/回退呈现（保真缺口，
        见任务报告）；
     ② 声明图标：市场条目 icon，渲染前过 safeAnnounceIcon 白名单
        （仅 https:// 与 data:image/）；
     ③ 回退：首字符 + hashGradient 哈希渐变（与壳层 UserAvatar 同一套色板）。
     本组件只负责「里面画什么」：尺寸/圆角由调用方既有类名控制。 -->
<template>
  <span class="app-icon-root" :style="rootStyle">
    <img v-if="imgSrc" :src="imgSrc" class="app-icon-img" alt="" @error="onImgError" />
    <template v-else>{{ fallbackChar }}</template>
  </span>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { hashGradient, safeAnnounceIcon } from '../market-store';

/** 图标渲染所需的最小条目形状（市场条目或其子集） */
export type MarketIconItem = { id?: string; name?: string; icon?: string };

export default defineComponent({
  name: 'AppIcon',
  props: {
    item: { type: Object as PropType<MarketIconItem | null>, default: null },
    /** 无条目时的应用 id（无市场条目的场景） */
    appId: { type: String, default: '' },
    /** 可选像素尺寸覆盖（缺省由调用方类名控制尺寸） */
    size: { type: Number, default: 0 }
  },
  setup(props) {
    const appId = computed(() => props.item?.id ?? props.appId);
    const appName = computed(() => props.item?.name ?? '');
    const fallbackChar = computed(() => (appName.value || appId.value).slice(0, 1) || '?');

    /** 声明图标（过白名单；空串 = 无） */
    const declaredIcon = computed(() => safeAnnounceIcon(props.item?.icon ?? ''));
    /** <img> 加载失败标记：回退链 ③（与壳层同口径：onerror 不再回落） */
    const imgFailed = ref(false);

    const imgSrc = computed(() => (imgFailed.value ? '' : declaredIcon.value));
    const onImgError = () => {
      imgFailed.value = true;
    };

    const rootStyle = computed(() => {
      const style: Record<string, string> = {};
      if (props.size > 0) {
        style.width = `${props.size}px`;
        style.height = `${props.size}px`;
      }
      // 回退态才铺哈希渐变底色（img 态图标自含底色）
      if (!imgSrc.value) {
        style.background = hashGradient(appId.value || appName.value);
      }
      return style;
    });

    return { imgSrc, fallbackChar, rootStyle, onImgError };
  }
});
</script>

<style scoped>
.app-icon-root {
  overflow: hidden;
  flex-shrink: 0;
}

.app-icon-img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: inherit;
}
</style>
