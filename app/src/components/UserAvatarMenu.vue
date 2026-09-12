<!-- 当前身份头像按钮（rail 底部「我的」入口）：**始终展示根身份（个人身份），不随空间切换**
     （走查决策：rail 是系统根级区域——切空间时「我的」不要变化）。组织空间下的域内身份入口在
     顶栏「当前身份」菜单的组织身份对话框（D2，OrgIdentityDialog）。
     头像取数统一走 stores/avatar-sources（与个人设置页头/空间切换器等同源）。
     宽栏（.rail.expanded）时头像右侧显示昵称+副标题（个人设置），窄栏只显示头像 -->
<template>
  <button class="identity-trigger" :title="`${source.name}：我的资料`" @click="emit('open-profile')">
    <UserAvatar :root-id="source.seed" :nickname="source.name" :avatar="source.image" :size="avatarSize" />
    <span class="identity-meta">
      <b class="identity-name">{{ source.name }}</b>
      <span class="identity-subtitle"><slot name="subtitle">{{ subtitle }}</slot></span>
    </span>
  </button>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import UserAvatar from './UserAvatar.vue';
import { personalAvatarSource } from '../stores/avatar-sources';

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
    /** 恒为根身份：切空间不变（组织身份不再随空间顶替此位） */
    const source = computed(() => personalAvatarSource());
    const subtitle = computed(() => '个人设置');

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
