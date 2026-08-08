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

    <!-- 输入行：移动端=输入框+发送按钮横排（微信式）；桌面端=输入框（发送按钮在底部独立行） -->
    <div class="msg-input-row">
      <el-input
        ref="inputRef"
        :model-value="modelValue"
        :type="isMobileLayout ? 'text' : 'textarea'"
        :autosize="isMobileLayout ? undefined : { minRows: 3, maxRows: 8 }"
        resize="none"
        :placeholder="disabled ? disabledHint : isMobileLayout ? '输入消息' : '输入消息，Enter 发送，Shift+Enter 换行'"
        :disabled="disabled"
        @update:model-value="(v: string) => $emit('update:modelValue', v)"
        @keydown.enter.exact.prevent="onSend"
      />

      <!-- 移动端：输入框右侧发送按钮（微信式）；桌面端保留底部发送行 -->
      <el-button
        v-if="isMobileLayout"
        type="primary"
        class="msg-mobile-send"
        :disabled="disabled || !modelValue.trim()"
        @click="onSend"
      >
        发送
      </el-button>
    </div>

    <div v-if="!isMobileLayout" class="msg-send-row">
      <el-button type="primary" :disabled="disabled || !modelValue.trim()" @click="onSend">发送</el-button>
    </div>
  </footer>
</template>

<script lang="ts">
import { defineComponent, ref, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { ChatDotRound, Close, Folder, Microphone, Picture } from '@element-plus/icons-vue';
import { isMobileLayout } from '../../stores/ui-layout';
import type { QuoteRef } from '../../stores/messages';

const EMOJIS = [
  '😀', '😄', '😂', '🤣', '😊', '😍', '🤔', '😅',
  '😭', '😤', '👍', '👎', '👌', '🙏', '👏', '💪',
  '🎉', '❤️', '🔥', '✨', '⭐', '☀️', '🌙', '☕'
];

export default defineComponent({
  name: 'MessageInput',
  // 图标组件显式注册：el-icon 插槽内的 <Close/> 需经 components 解析，
  // 仅 setup return 在 dev 模板编译下偶发无法解析（Failed to resolve component 告警）
  components: { Close },
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

    return { inputRef, onSend, insertEmoji, placeholder, EMOJIS, isMobileLayout, Microphone, Picture, Folder, ChatDotRound, Close };
  }
});
</script>
