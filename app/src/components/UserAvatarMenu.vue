<!-- 当前身份头像按钮（rail 顶部第一项）：个人空间=个人身份，组织空间=组织身份（随空间切换）。
     点击直接把内容区切到「我的资料」页；组织身份的编辑入口在 MinePage「组织身份」模块（OrgIdentityModule）。
     头像取数统一走 stores/avatar-sources（与个人设置页头/空间切换器等同源）。
     宽栏（.rail.expanded）时头像右侧显示昵称+副标题（个人设置/组织身份），窄栏只显示头像 -->
<template>
  <button class="identity-trigger" :title="`${source.name}：我的资料`" @click="emit('open-profile')">
    <UserAvatar :root-id="source.seed" :nickname="source.name" :avatar="source.image" :size="avatarSize" />
    <span class="identity-meta">
      <b class="identity-name">{{ source.name }}</b>
      <span class="identity-subtitle">{{ subtitle }}</span>
    </span>
  </button>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import UserAvatar from './UserAvatar.vue';
import { currentSpace, currentSpaceOrgId } from '../stores/current-space';
import { getOrgIdentity } from '../stores/org-identity';
import { orgIdentityAvatarSource, personalAvatarSource } from '../stores/avatar-sources';

export default defineComponent({
  name: 'UserAvatarMenu',
  components: {
    UserAvatar
  },
  props: {
    /** 触发器头像尺寸（rail 顶部比图标项稍大） */
    avatarSize: { type: Number, default: 30 }
  },
  emits: ['open-profile'],
  setup(_, { emit }) {
    /** 组织空间且未开「使用个人身份」时展示组织身份，否则展示个人身份 */
    const isOrgIdentity = computed(
      () => currentSpace.value.type === 'org' && !getOrgIdentity(currentSpaceOrgId.value).usePersonalIdentity
    );
    const source = computed(() => {
      if (isOrgIdentity.value) {
        return orgIdentityAvatarSource(currentSpaceOrgId.value);
      }
      return personalAvatarSource();
    });
    // 宽栏副标题：与 MinePage 页头口径一致
    const subtitle = computed(() => (isOrgIdentity.value ? '组织身份' : '个人设置'));

    return {
      source,
      subtitle,
      emit
    };
  }
});
</script>

<style scoped>
.identity-trigger {
  border: 0;
  background: transparent;
  cursor: pointer;
  padding: 2px;
  border-radius: 50%;
  display: flex;
  -webkit-app-region: no-drag;
  transition: transform 0.15s ease;
}

.identity-trigger:hover {
  transform: scale(1.06);
}

/* 昵称 + 副标题：窄栏隐藏，宽栏（.rail.expanded）显示在头像右侧 */
.identity-meta {
  display: none;
  flex-direction: column;
  align-items: flex-start;
  min-width: 0;
  text-align: left;
}

.identity-name {
  max-width: 100%;
  font-size: 14px;
  font-weight: 600;
  line-height: 1.3;
  color: var(--spark-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.identity-subtitle {
  font-size: 12px;
  line-height: 1.3;
  color: var(--spark-text-3);
}

/* 宽栏：触发器变为整行（头像 + 文字），圆角矩形 hover 与 rail-item 一致 */
.rail.expanded .identity-trigger {
  width: 100%;
  align-items: center;
  gap: 10px;
  padding: 6px 12px;
  border-radius: var(--spark-radius-l);
}

.rail.expanded .identity-trigger:hover {
  background: var(--spark-rail-item-hover);
  transform: none;
}

.rail.expanded .identity-meta {
  display: flex;
}
</style>
