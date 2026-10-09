<!-- 组织头像（插件自足版，与壳层 OrgAvatar 同语义）：有上传 logo 显示图片，
     没有则按 orgId 哈希配色 + 组织名首字生成；圆角矩形（与个人圆形头像区分） -->
<template>
  <span class="org-avatar" :style="boxStyle">
    <img v-if="avatar" :src="avatar" :alt="name" class="org-avatar-img" />
    <span v-else class="org-avatar-char" :style="{ fontSize: `${Math.round(size * 0.44)}px` }">{{ firstChar }}</span>
  </span>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { hashGradient } from '../org-store';

export default defineComponent({
  name: 'OrgAvatar',
  props: {
    /** 自动生成配色的哈希种子 */
    orgId: { type: String, default: '' },
    /** 自动头像取首字 */
    name: { type: String, default: '' },
    /** 显式 logo（dataURL）；空串 = 自动头像 */
    avatar: { type: String, default: '' },
    size: { type: Number, default: 36 }
  },
  setup(props) {
    const firstChar = computed(() => (props.name.trim() || '组').slice(0, 1));
    const boxStyle = computed(() => ({
      width: `${props.size}px`,
      height: `${props.size}px`,
      background: props.avatar ? 'transparent' : hashGradient(props.orgId || props.name)
    }));
    return { firstChar, boxStyle };
  }
});
</script>

<style scoped>
.org-avatar {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--spark-radius-m);
  overflow: hidden;
  flex-shrink: 0;
  color: #fff;
  user-select: none;
}

.org-avatar-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.org-avatar-char {
  line-height: 1;
  font-weight: 600;
}
</style>
