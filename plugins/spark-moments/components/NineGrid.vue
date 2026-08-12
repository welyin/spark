<!--
  朋友圈插件（spark-moments）· 九宫格图片展示（timeline.md §4.3）。
  布局：1 张单图（原比例 ≤200×260）；2/4 张两列；3/5–9 张三列。
  内容用缩略图 thumbHash（KB 级即时出图）；点击经 readBlob 拉全图进 LightboxPreview。
  缩略图 pending → 色块占位 + 图片图标（内核已登记拉取意图，稍后重读）。
-->
<template>
  <div class="nine-grid" :class="`grid-${images.length}`">
    <div
      v-for="(img, index) in images"
      :key="img.hash"
      class="cell"
      :class="{ single: images.length === 1 }"
      @click="$emit('preview', index)"
    >
      <ThumbImage :image="img" />
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import ThumbImage from './ThumbImage.vue';
import type { MomentsImage } from '../model';

export default defineComponent({
  name: 'NineGrid',
  components: { ThumbImage },
  props: {
    images: { type: Array as () => MomentsImage[], required: true }
  },
  emits: ['preview']
});
</script>

<style scoped>
.nine-grid {
  display: grid;
  gap: 4px;
  border-radius: 4px;
  overflow: hidden;
}
.grid-1 { grid-template-columns: 1fr; }
.grid-2 { grid-template-columns: repeat(2, 1fr); }
.grid-4 { grid-template-columns: repeat(2, 1fr); }
.grid-3, .grid-5, .grid-6, .grid-7, .grid-8, .grid-9 { grid-template-columns: repeat(3, 1fr); }
.cell {
  aspect-ratio: 1 / 1;
  width: 100%;
  overflow: hidden;
}
.cell.single {
  aspect-ratio: auto;
  max-width: 200px;
  max-height: 260px;
}
</style>
