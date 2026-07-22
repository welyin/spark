<template>
  <div class="avatar-picker">
    <UserAvatar :nickname="nickname" :avatar="modelValue" :size="size" />
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
      emit
    };
  }
});
</script>

<style scoped src="../styles/components/avatar-picker.css"></style>
