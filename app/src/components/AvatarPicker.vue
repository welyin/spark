<template>
  <div class="avatar-picker">
    <!-- 有上传图或昵称时预览（自动配色头像）；否则灰底相机占位，避免「未」字彩色圆 -->
    <UserAvatar v-if="modelValue || nickname.trim()" :root-id="seed" :nickname="nickname" :avatar="modelValue" :size="size" />
    <span v-else class="avatar-picker-placeholder" :style="{ width: `${size}px`, height: `${size}px` }">
      <el-icon :size="Math.round(size * 0.4)"><Camera /></el-icon>
    </span>
    <div class="avatar-picker-actions">
      <el-button size="small" :disabled="disabled" @click="triggerSelect">上传图片</el-button>
      <el-button v-if="modelValue" size="small" text type="danger" :disabled="disabled" @click="emit('update:modelValue', '')">
        移除
      </el-button>
    </div>
    <input ref="fileInput" type="file" accept="image/*" class="hidden-input" @change="onChange" />
  </div>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { Camera } from '@element-plus/icons-vue';
import UserAvatar from './UserAvatar.vue';
import { fileToAvatarDataUrl } from '../utils/avatar';

export default defineComponent({
  name: 'AvatarPicker',
  components: {
    UserAvatar
  },
  props: {
    /** 头像 dataURL；空串表示使用自动头像 */
    modelValue: {
      type: String,
      default: ''
    },
    /** 预览自动头像时取首字与配色的昵称 */
    nickname: {
      type: String,
      default: ''
    },
    /** 自动头像配色种子（rootId 或 rootId@orgId）；缺省按昵称哈希（注册预览等无身份场景） */
    seed: {
      type: String,
      default: ''
    },
    size: {
      type: Number,
      default: 56
    },
    disabled: {
      type: Boolean,
      default: false
    }
  },
  emits: ['update:modelValue'],
  setup(_, { emit }) {
    const fileInput = ref<HTMLInputElement | null>(null);

    const triggerSelect = () => {
      fileInput.value?.click();
    };

    const onChange = async (event: Event) => {
      const input = event.target as HTMLInputElement;
      const file = input.files?.[0];
      input.value = '';
      if (!file) {
        return;
      }
      try {
        emit('update:modelValue', await fileToAvatarDataUrl(file));
      } catch (error) {
        ElMessage.warning(error instanceof Error ? error.message : '图片读取失败，请换一张重试');
      }
    };

    return {
      fileInput,
      triggerSelect,
      onChange,
      Camera,
      emit
    };
  }
});
</script>

<style scoped src="../styles/components/avatar-picker.css"></style>
