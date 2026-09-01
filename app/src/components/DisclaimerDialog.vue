<!-- 免责声明弹窗：首次启动（first-run）须「我已阅读并同意」方可进入，不可跳过；
     设置页常驻入口以只读模式复用（可关闭）。文案统一取自 utils/disclaimer -->
<template>
  <el-dialog
    :model-value="modelValue"
    :title="DISCLAIMER_TITLE"
    :width="dialogWidth"
    :show-close="!firstRun"
    :close-on-click-modal="false"
    :close-on-press-escape="!firstRun"
    align-center
    @update:model-value="onUpdate"
  >
    <div class="disclaimer-body">
      <p v-for="(text, index) in DISCLAIMER_PARAGRAPHS" :key="index" class="disclaimer-paragraph">
        {{ text }}
      </p>
    </div>
    <template #footer>
      <el-button v-if="firstRun" type="primary" @click="onAccept">我已阅读并同意</el-button>
      <el-button v-else @click="onUpdate(false)">关闭</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { DISCLAIMER_PARAGRAPHS, DISCLAIMER_TITLE } from '../utils/disclaimer';
import { isMobileLayout } from '../stores/ui-layout';

export default defineComponent({
  name: 'DisclaimerDialog',
  props: {
    modelValue: { type: Boolean, required: true },
    /** 首次启动模式：不可跳过、无关闭按钮，只能同意 */
    firstRun: { type: Boolean, default: false }
  },
  emits: ['update:modelValue', 'accept'],
  setup(_, { emit }) {
    const dialogWidth = computed(() => (isMobileLayout.value ? '92%' : '480px'));

    const onUpdate = (visible: boolean) => emit('update:modelValue', visible);
    const onAccept = () => {
      emit('accept');
      emit('update:modelValue', false);
    };

    return { DISCLAIMER_TITLE, DISCLAIMER_PARAGRAPHS, dialogWidth, onUpdate, onAccept };
  }
});
</script>

<style scoped>
.disclaimer-body {
  max-height: 50vh;
  overflow-y: auto;
}

.disclaimer-paragraph {
  margin: 0;
  font-size: 13px;
  line-height: 1.8;
  color: var(--spark-text-1, #303133);
}

.disclaimer-paragraph + .disclaimer-paragraph {
  margin-top: 10px;
}
</style>
