<template>
  <span class="user-avatar" :style="{ width: `${size}px`, height: `${size}px`, fontSize: `${fontSize}px` }">
    <img v-if="avatar" :src="avatar" :alt="displayName" class="user-avatar-img" />
    <span v-else class="user-avatar-auto" :style="{ background: autoBackground }">{{ initial }}</span>
  </span>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { hashGradient } from '../utils/palette';

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
    const autoBackground = computed(() => hashGradient(props.rootId || props.nickname || 'spark'));

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
