<!-- 头像选择器（插件自足版，与壳层 AvatarPicker 同语义）：上传图片（canvas
     压到 ≤256px 转 dataURL，控制同步载荷体积）/ 移除回退自动头像 -->
<template>
  <div class="avatar-picker">
    <OrgAvatar v-if="modelValue || nickname.trim()" :org-id="seed" :name="nickname" :avatar="modelValue" :size="size" />
    <span v-else class="avatar-picker-placeholder" :style="{ width: `${size}px`, height: `${size}px` }">
      <el-icon :size="Math.round(size * 0.4)"><Camera /></el-icon>
    </span>
    <div class="avatar-picker-actions">
      <el-button size="small" :disabled="disabled" @click="triggerSelect">上传图片</el-button>
      <el-button
        v-if="modelValue"
        size="small"
        text
        type="danger"
        :disabled="disabled"
        @click="onRemove"
      >移除</el-button>
    </div>
    <input ref="fileInput" type="file" accept="image/*" class="hidden-input" @change="onChange" />
  </div>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { Camera } from '@element-plus/icons-vue';
import OrgAvatar from './OrgAvatar.vue';

/** 图片压到最长边 256px 的 JPEG/PNG dataURL（组织 logo 走同步流量，必须紧凑） */
const MAX_EDGE = 256;

async function fileToAvatarDataUrl(file: File): Promise<string> {
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error('图片读取失败'));
    reader.readAsDataURL(file);
  });
  const image = await new Promise<HTMLImageElement>((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('图片解码失败'));
    img.src = dataUrl;
  });
  const scale = Math.min(1, MAX_EDGE / Math.max(image.width, image.height, 1));
  if (scale >= 1) {
    return dataUrl;
  }
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(image.width * scale));
  canvas.height = Math.max(1, Math.round(image.height * scale));
  const ctx = canvas.getContext('2d');
  if (!ctx) {
    return dataUrl;
  }
  ctx.drawImage(image, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL('image/png');
}

export default defineComponent({
  name: 'AvatarPicker',
  components: { OrgAvatar, Camera },
  props: {
    /** 头像 dataURL；空串表示使用自动头像 */
    modelValue: { type: String, default: '' },
    /** 预览自动头像时取首字与配色的昵称 */
    nickname: { type: String, default: '' },
    /** 自动头像配色种子（orgId 等） */
    seed: { type: String, default: '' },
    size: { type: Number, default: 56 },
    disabled: { type: Boolean, default: false }
  },
  emits: ['update:modelValue'],
  setup(_, { emit }) {
    const fileInput = ref<HTMLInputElement | null>(null);

    const triggerSelect = () => {
      fileInput.value?.click();
    };

    const onRemove = () => emit('update:modelValue', '');

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
        ElMessage.error(`${error}`);
      }
    };

    return { fileInput, triggerSelect, onRemove, onChange };
  }
});
</script>

<style scoped>
.avatar-picker {
  display: flex;
  align-items: center;
  gap: 12px;
}

.avatar-picker-placeholder {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--spark-radius-m);
  background: var(--spark-bg-hover);
  color: var(--spark-text-3);
}

.avatar-picker-actions {
  display: flex;
  gap: 8px;
}

.hidden-input {
  display: none;
}
</style>
