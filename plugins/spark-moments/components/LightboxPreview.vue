<!--
  朋友圈插件（spark-moments）· 全屏图片预览（detail.md §3）。
  iframe 内 fixed 全幅遮罩（黑底 95%）；多图左右滑切换（桌面 ←/→ + 箭头）；
  完整图 readBlob(hash) 按需拉取，pending → spinner，超时失败 → 重试。
-->
<template>
  <div class="lightbox" @click.self="$emit('close')">
    <button type="button" class="close" @click="$emit('close')">✕</button>
    <div class="viewer">
      <img
        v-if="src"
        :src="src"
        alt=""
        class="full-img"
        :key="index"
        @click="next"
      />
      <div v-else-if="failed" class="state">
        <span class="retry" @click="load">加载失败，重试</span>
      </div>
      <div v-else class="state">图片拉取中…</div>
    </div>
    <div v-if="images.length > 1" class="indicator">{{ index + 1 }}/{{ images.length }}</div>
    <button v-if="images.length > 1 && index > 0" class="nav prev" @click="prev">‹</button>
    <button v-if="images.length > 1 && index < images.length - 1" class="nav next" @click="next">›</button>
  </div>
</template>

<script lang="ts">
import { defineComponent, onMounted, onUnmounted, ref } from 'vue';
import { ensurePluginSDK } from '../../../packages/plugin-sdk/src';
import type { MomentsImage } from '../model';

const LOAD_TIMEOUT_MS = 30_000;

export default defineComponent({
  name: 'LightboxPreview',
  props: {
    images: { type: Array as () => MomentsImage[], required: true },
    start: { type: Number, default: 0 }
  },
  emits: ['close'],
  setup(props, { emit }) {
    const index = ref(props.start);
    const src = ref('');
    const failed = ref(false);
    let timer: ReturnType<typeof setTimeout> | null = null;

    const load = async () => {
      const img = props.images[index.value];
      if (!img) return;
      src.value = '';
      failed.value = false;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { failed.value = true; }, LOAD_TIMEOUT_MS);
      try {
        const sdk = await ensurePluginSDK();
        const result = await sdk.data.readBlob(img.hash);
        if (result.status === 'ready') {
          src.value = `data:${img.mime};base64,${result.data}`;
        } else {
          // pending：内核已登记拉取意图，轮询重读
          setTimeout(load, 1000);
        }
      } catch {
        failed.value = true;
      }
    };

    const prev = () => { index.value = Math.max(0, index.value - 1); load(); };
    const next = () => { index.value = Math.min(props.images.length - 1, index.value + 1); load(); };

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') emit('close');
      if (e.key === 'ArrowLeft') prev();
      if (e.key === 'ArrowRight') next();
    };

    onMounted(() => {
      load();
      window.addEventListener('keydown', onKey);
    });
    onUnmounted(() => {
      if (timer) clearTimeout(timer);
      window.removeEventListener('keydown', onKey);
    });

    return { index, src, failed, load, prev, next };
  }
});
</script>

<style scoped>
.lightbox {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.95);
  z-index: 100;
  display: flex;
  align-items: center;
  justify-content: center;
}
.close {
  position: absolute;
  top: 16px;
  right: 16px;
  width: 36px;
  height: 36px;
  border-radius: 50%;
  border: none;
  background: rgba(255, 255, 255, 0.15);
  color: #fff;
  font-size: 18px;
  cursor: pointer;
  z-index: 10;
}
.viewer {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}
.full-img {
  max-width: 92%;
  max-height: 92%;
  object-fit: contain;
  cursor: pointer;
}
.state { color: #cbd5e1; }
.retry { cursor: pointer; text-decoration: underline; }
.indicator {
  position: absolute;
  bottom: 20px;
  left: 50%;
  transform: translateX(-50%);
  color: #fff;
  font-size: 14px;
}
.nav {
  position: absolute;
  top: 50%;
  transform: translateY(-50%);
  width: 44px;
  height: 44px;
  border-radius: 50%;
  border: none;
  background: rgba(255, 255, 255, 0.15);
  color: #fff;
  font-size: 26px;
  cursor: pointer;
}
.nav.prev { left: 16px; }
.nav.next { right: 16px; }
</style>
