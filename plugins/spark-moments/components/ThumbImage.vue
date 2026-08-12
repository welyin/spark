<!--
  朋友圈插件（spark-moments）· 缩略图加载组件（timeline.md §6.2 图片占位）。
  响应用内建 blob 的 ready / pending 两态：
  - ready → 直接渲染缩略图；
  - pending → 色块占位 + 图片图标，重读 3 次后仍 pending 显示「重试」。
  内核 blob 策略（PC eager / 手机 lazy）对 UI 透明，本组件只响应两态。
-->
<template>
  <div class="thumb" :style="{ background: 'var(--spark-bg-hover, #f1f5f9)' }">
    <img
      v-if="src"
      :src="src"
      alt=""
      class="img"
      @error="onError"
    />
    <div v-else-if="failed" class="fallback">
      <span class="retry" @click="load">重试</span>
    </div>
    <div v-else class="fallback">
      <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.5" opacity="0.5">
        <rect x="3" y="3" width="18" height="18" rx="3" />
        <circle cx="8.5" cy="8.5" r="1.5" />
        <path d="M21 15l-5-5-9 9" />
      </svg>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import { ensurePluginSDK } from '../../../packages/plugin-sdk/src';
import type { MomentsImage } from '../model';

const MAX_RETRY = 3;

export default defineComponent({
  name: 'ThumbImage',
  props: {
    image: { type: Object as () => MomentsImage, required: true }
  },
  setup(props) {
    const src = ref('');
    const failed = ref(false);
    let retry = 0;

    const load = async () => {
      try {
        const sdk = await ensurePluginSDK();
        const result = await sdk.data.readBlob(props.image.thumbHash);
        if (result.status === 'ready') {
          src.value = `data:${props.image.mime};base64,${result.data}`;
          failed.value = false;
        } else if (retry < MAX_RETRY) {
          retry += 1;
          // 内核已登记拉取意图，稍后重读（指数退避）
          setTimeout(load, retry * 500);
        } else {
          failed.value = true;
        }
      } catch {
        failed.value = true;
      }
    };

    const onError = () => {
      src.value = '';
      failed.value = true;
    };

    onMounted(load);

    return { src, failed, load, onError };
  }
});
</script>

<style scoped>
.thumb {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--spark-text-3, #94a3b8);
}
.cell.single .thumb { border-radius: 4px; }
.img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.cell.single .img { object-fit: contain; }
.fallback {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  height: 100%;
  font-size: 12px;
  color: var(--spark-text-3, #94a3b8);
}
.retry { cursor: pointer; text-decoration: underline; }
</style>
