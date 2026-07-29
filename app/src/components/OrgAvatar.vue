<!-- 组织头像：有上传 logo 显示图片，没有则按 orgId 哈希配色 + 组织名首字生成（复用 UserAvatar 的配色算法）。
     形状为圆角矩形（钉钉群头像观感），与个人头像（圆形）形成明确的视觉区分 -->
<template>
  <UserAvatar class="org-avatar" :root-id="orgId" :nickname="name" :avatar="resolvedAvatar" :size="size" />
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import UserAvatar from './UserAvatar.vue';
import { orgAvatars } from '../stores/org-avatars';

export default defineComponent({
  name: 'OrgAvatar',
  components: {
    UserAvatar
  },
  props: {
    /** 自动生成配色的哈希种子 */
    orgId: {
      type: String,
      default: ''
    },
    /** 自动生成头像取首字 */
    name: {
      type: String,
      default: ''
    },
    /**
     * 显式 logo（dataURL）；缺省（undefined）时从 org-avatars store 按 orgId 查，
     * 显式传空串表示强制使用自动头像（如创建对话框预览）。
     */
    avatar: {
      type: String,
      default: undefined
    },
    size: {
      type: Number,
      default: 36
    }
  },
  setup(props) {
    const resolvedAvatar = computed(() => props.avatar ?? orgAvatars.value[props.orgId] ?? '');
    return { resolvedAvatar };
  }
});
</script>

<!-- 非 scoped：需覆盖 UserAvatar scoped 样式里的 border-radius: 50%（span 选择器提权稳赢），
     方形对自动头像与上传 logo 两种形态同时生效（容器 overflow: hidden 裁切图片） -->
<style>
span.org-avatar.user-avatar {
  border-radius: var(--spark-radius-m);
}
</style>
