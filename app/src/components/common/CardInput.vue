<template>
  <div class="card-input">
    <div
      class="card-upload"
      :class="{ ok: status === 'ok' }"
      @click="openPicker"
    >
      <input
        ref="fileInput"
        type="file"
        accept="image/*"
        class="card-file-input"
        @change="onPick"
      />
      <template v-if="status === 'idle'">
        <div class="card-upload-icon">＋</div>
        <div class="card-upload-text">点击上传对方名片图片</div>
        <div class="card-upload-hint">支持 PNG / JPG，自动识别名片内容</div>
      </template>
      <template v-else-if="status === 'ok'">
        <div class="card-upload-icon ok">✓</div>
        <div class="card-upload-text">{{ fileName }}</div>
        <div class="card-upload-hint ok-text">已识别名片，点击可更换</div>
      </template>
      <template v-else>
        <div class="card-upload-icon">＋</div>
        <div class="card-upload-text">{{ fileName }}</div>
        <div class="card-upload-hint warn-text">未识别到名片二维码，请换一张，或在下方粘贴名片内容</div>
      </template>
    </div>
    <label class="field-label">或粘贴对方名片内容</label>
    <textarea
      v-model="text"
      class="input card-textarea"
      placeholder="粘贴对方发来的名片内容，系统会自动识别其中的身份信息"
    ></textarea>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';
import { decodeCardImage } from '../../utils/card';

export default defineComponent({
  name: 'CardInput',
  props: {
    modelValue: { type: String, default: '' },
  },
  emits: ['update:modelValue'],
  data() {
    return {
      text: '',
      decoded: '',
      fileName: '',
      status: 'idle' as 'idle' | 'ok' | 'fail',
    };
  },
  watch: {
    decoded() {
      this.emitValue();
    },
    text() {
      this.emitValue();
    },
    modelValue(val: string) {
      if (!val && (this.decoded || this.text)) {
        this.reset();
      }
    },
  },
  methods: {
    reset() {
      this.decoded = '';
      this.text = '';
      this.fileName = '';
      this.status = 'idle';
      const input = this.$refs.fileInput as HTMLInputElement | undefined;
      if (input) input.value = '';
    },
    openPicker() {
      (this.$refs.fileInput as HTMLInputElement | undefined)?.click();
    },
    emitValue() {
      this.$emit('update:modelValue', this.decoded || this.text.trim());
    },
    async onPick(e: Event) {
      const input = e.target as HTMLInputElement;
      const file = input.files?.[0];
      if (!file) return;
      this.fileName = file.name;
      const decoded = await decodeCardImage(file);
      this.decoded = decoded;
      this.status = decoded ? 'ok' : 'fail';
    },
  },
});
</script>

<style scoped>
.card-upload {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 18px 14px;
  border: 1px dashed var(--spark-border);
  border-radius: var(--spark-radius-s);
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s;
}

.card-upload:hover {
  border-color: var(--spark-primary);
  background: var(--spark-primary-light);
}

.card-upload.ok {
  border-style: solid;
  border-color: var(--spark-primary);
}

.card-upload-icon {
  font-size: 22px;
  line-height: 1;
  color: var(--spark-text-2);
}

.card-upload-icon.ok {
  color: var(--spark-primary);
}

.card-upload-text {
  font-size: 13px;
  color: var(--spark-text-1);
}

.card-upload-hint {
  font-size: 12px;
  color: var(--spark-text-3);
}

.card-file-input {
  display: none;
}

.ok-text {
  color: var(--spark-primary);
}

.warn-text {
  color: var(--spark-warning);
}

.field-label {
  display: block;
  margin-top: 12px;
  margin-bottom: 6px;
  font-size: 13px;
  color: var(--spark-text-2);
}

.card-textarea {
  width: 100%;
  min-height: 96px;
  resize: vertical;
}
</style>
