<template>
  <span class="user-avatar" :style="{ width: `${size}px`, height: `${size}px`, fontSize: `${fontSize}px` }">
    <img v-if="avatar" :src="avatar" :alt="displayName" class="user-avatar-img" />
    <span v-else class="user-avatar-auto" :style="{ background: autoBackground }">{{ initial }}</span>
  </span>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';

/** 自动头像调色板（与品牌蓝协调的 8 组渐变），按 rootId 哈希确定选取 */
const PALETTES: Array<[string, string]> = [
  ['#3296fa', '#2b83dd'],
  ['#7b61ff', '#5a3fd6'],
  ['#00b8a9', '#008577'],
  ['#f7b500', '#e08600'],
  ['#f54a45', '#cf352f'],
  ['#eb2f96', '#c41d7f'],
  ['#34c19b', '#1f9c7c'],
  ['#ff7d00', '#e56a00']
];

export default defineComponent({
  name: 'UserAvatar',
  props: {
    rootId: {
      type: String,
      default: ''
    },
    nickname: {
      type: String,
      default: ''
    },
    avatar: {
      type: String,
      default: ''
    },
    size: {
      type: Number,
      default: 36
    }
  },
  setup(props) {
    const displayName = computed(() => props.nickname.trim() || '未命名用户');

    const initial = computed(() => {
      const first = [...displayName.value][0] ?? '用';
      return /^[a-z]$/i.test(first) ? first.toUpperCase() : first;
    });

    // 同一 rootId 恒得同一配色；无 rootId（注册预览）时按昵称哈希
    const autoBackground = computed(() => {
      const seed = props.rootId || props.nickname || 'spark';
      let hash = 0;
      for (const char of seed) {
        hash = (hash * 31 + (char.codePointAt(0) ?? 0)) >>> 0;
      }
      const [from, to] = PALETTES[hash % PALETTES.length];
      return `linear-gradient(135deg, ${from}, ${to})`;
    });

    const fontSize = computed(() => Math.max(11, Math.round(props.size * 0.44)));

    return {
      displayName,
      initial,
      autoBackground,
      fontSize
    };
  }
});
</script>

<style scoped src="../styles/components/user-avatar.css"></style>
