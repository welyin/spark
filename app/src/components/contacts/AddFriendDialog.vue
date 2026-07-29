<!-- 职责：添加朋友对话框（个人空间，ui-contacts §4.1）：
     PC 端入口——上传/粘贴对方名片（对应「个人设置 → 我的名片」：二维码名片图片或名片内容文本）；
     扫码入口仅移动端提供 -->
<template>
  <el-dialog v-model="visible" title="添加朋友" width="480px">
    <!-- TODO(mock): 添加方式为表单 UI + 写 mock store；真实流程为发送请求、对方确认后成为朋友（§4.1 双向确认） -->
    <!-- TODO(mock): 仅本地模式下请求无法送达对方，前端先行拦截（真实实现应由内核拒绝或排队） -->
    <el-alert
      v-if="isLocalOnly"
      title="当前为仅本地模式，无法发送添加请求"
      type="warning"
      :closable="false"
      show-icon
      class="add-friend-local-alert"
    />
    <p class="hint">让对方打开「个人设置 → 我的名片」，把二维码名片图片或名片内容发给你。</p>
    <CardInput v-model="cardRaw" />
    <el-form label-position="top">
      <el-form-item label="验证消息（可选）">
        <el-input v-model="message" placeholder="向对方说明你是谁" @keydown.enter.prevent="submit" />
      </el-form-item>
    </el-form>
    <template #footer>
      <el-button @click="visible = false">取消</el-button>
      <el-button type="primary" @click="submit">发送添加请求</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import CardInput from '../common/CardInput.vue';
import { parseCard } from '../../utils/card';
import { useNetworkStatus } from '../../stores/network-status';

export default defineComponent({
  name: 'AddFriendDialog',
  components: { CardInput },
  props: {
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue', 'submit'],
  setup(props, { emit }) {
    const cardRaw = ref('');
    const message = ref('');
    const { isLocalOnly } = useNetworkStatus();

    const visible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const submit = () => {
      // TODO(mock): 仅本地拦截为前端演示逻辑；真实实现应由内核拒绝或排队
      if (isLocalOnly.value) {
        ElMessage.warning('仅本地模式下无法发送邀请');
        return;
      }
      const raw = cardRaw.value.trim();
      if (!raw) {
        ElMessage.warning('请上传名片图片或粘贴名片内容');
        return;
      }
      const card = parseCard(raw);
      if (!card) {
        ElMessage.warning('未从内容中识别到有效的身份 ID，请确认名片内容完整');
        return;
      }
      emit('submit', {
        rootId: card.rootId,
        raw,
        source: '名片',
        message: message.value.trim()
      });
      cardRaw.value = '';
      message.value = '';
      visible.value = false;
    };

    return { visible, cardRaw, message, submit, isLocalOnly };
  }
});
</script>

<style scoped>
.add-friend-local-alert {
  margin-bottom: 12px;
}
</style>
