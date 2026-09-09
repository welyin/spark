<template>
  <el-dialog v-model="visible" title="全局搜索" width="min(640px, 90vw)" append-to-body @opened="focusSearch">
    <div ref="searchRoot" class="desktop-search"><GlobalSearch /></div>
  </el-dialog>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import GlobalSearch from '../GlobalSearch.vue';

const visible = ref(false);
const searchRoot = ref<HTMLElement | null>(null);
const open = () => { visible.value = true; };
const focusSearch = () => searchRoot.value?.querySelector('input')?.focus();
const onKey = (event: KeyboardEvent) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
    event.preventDefault();
    open();
  }
};
const close = () => { visible.value = false; };
const navigationEvents = ['spark:open-chat', 'spark:open-plugin', 'spark:open-app', 'spark:switch-tab'];
onMounted(() => {
  window.addEventListener('spark:search', open);
  window.addEventListener('keydown', onKey);
  navigationEvents.forEach((name) => window.addEventListener(name, close));
});
onUnmounted(() => {
  window.removeEventListener('spark:search', open);
  window.removeEventListener('keydown', onKey);
  navigationEvents.forEach((name) => window.removeEventListener(name, close));
});
</script>

<style scoped>
.desktop-search { min-height: 300px; }
.desktop-search :deep(.global-search) { width: 100%; max-width: none; }
.desktop-search :deep(.global-search-dropdown) { position: static; max-height: 50vh; box-shadow: none; }
</style>