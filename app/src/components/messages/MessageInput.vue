<!-- 输入区（设计 §3.4）：多行输入、Enter 发送 / Shift+Enter 换行、表情面板、语音/图片/文件占位 -->
<template>
  <footer class="msg-input-area">
    <div v-if="quote" class="quote-bar">
      <span class="quote-bar-text">回复 {{ quote.senderName }}：{{ quote.preview }}</span>
      <el-icon class="quote-bar-close" :size="14" @click="$emit('cancel-quote')"><Close /></el-icon>
    </div>

    <div class="msg-toolbar">
      <el-tooltip content="语音消息（后续版本提供）" placement="top">
        <el-button text :icon="Microphone" @click="placeholder" />
      </el-tooltip>
      <el-tooltip content="发送图片（后续版本提供）" placement="top">
        <el-button text :icon="Picture" @click="placeholder" />
      </el-tooltip>
      <el-tooltip content="发送文件（后续版本提供）" placement="top">
        <el-button text :icon="Folder" @click="placeholder" />
      </el-tooltip>
      <el-popover placement="top-start" :width="280" trigger="click">
        <template #reference>
          <el-button text :icon="ChatDotRound" />
        </template>
        <div class="emoji-panel">
          <button v-for="emoji in EMOJIS" :key="emoji" class="emoji-item" @click="insertEmoji(emoji)">
            {{ emoji }}
          </button>
        </div>
      </el-popover>
    </div>

    <el-input
      ref="inputRef"
      :model-value="modelValue"
      type="textarea"
      :autosize="{ minRows: 3, maxRows: 8 }"
      resize="none"
      :placeholder="disabled ? disabledHint : '输入消息，Enter 发送，Shift+Enter 换行'"
      :disabled="disabled"
      @update:model-value="(v: string) => $emit('update:modelValue', v)"
      @keydown.enter.exact.prevent="onSend"
    />

    <div class="msg-send-row">
      <el-button type="primary" :disabled="disabled || !modelValue.trim()" @click="onSend">发送</el-button>
    </div>
  </footer>
</template>

<script lang="ts">
import { defineComponent, ref, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { ChatDotRound, Close, Folder, Microphone, Picture } from '@element-plus/icons-vue';
import type { QuoteRef } from '../../mock/messages';

const EMOJIS = [
  '😀', '😄', '😂', '🤣', '😊', '😍', '🤔', '😅',
  '😭', '😤', '👍', '👎', '👌', '🙏', '👏', '💪',
  '🎉', '❤️', '🔥', '✨', '⭐', '☀️', '🌙', '☕'
];

export default defineComponent({
  name: 'MessageInput',
  props: {
    modelValue: { type: String, default: '' },
    quote: { type: Object as PropType<QuoteRef | null>, default: null },
    disabled: { type: Boolean, default: false },
    /** 禁用态占位文案（系统通知/应用会话不支持回复） */
    disabledHint: { type: String, default: '系统通知会话不支持回复' }
  },
  emits: ['update:modelValue', 'send', 'cancel-quote'],
  setup(props, { emit }) {
    const inputRef = ref();

    function onSend() {
      const text = props.modelValue.trim();
      if (!text || props.disabled) return;
      emit('send', text);
    }

    function insertEmoji(emoji: string) {
      emit('update:modelValue', props.modelValue + emoji);
    }

    // 语音/图片/文件本期仅占位（设计 §3.4 工具按钮）
    function placeholder() {
      ElMessage.info('该功能将在后续版本提供');
    }

    return { inputRef, onSend, insertEmoji, placeholder, EMOJIS, Microphone, Picture, Folder, ChatDotRound, Close };
  }
});
</script>
