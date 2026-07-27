<!-- 职责：邀请码加入组织对话框 -->
<template>
  <el-dialog v-model="dialogVisible" title="通过邀请码加入" width="520px">
    <el-form label-position="top">
      <el-form-item label="邀请码">
        <el-input
          v-model="joinCode"
          type="textarea"
          :rows="4"
          placeholder="粘贴管理员分享给你的邀请码"
        />
      </el-form-item>
    </el-form>
    <p class="hint">加入前提：管理员已先将你的 RootID 录入组织成员。邀请码 24 小时内有效，用于连接管理员节点并拉取组织数据。</p>
    <template #footer>
      <el-button @click="dialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="joining" @click="onSubmit">
        {{ joining ? '加入中...' : '加入组织' }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';

export default defineComponent({
  name: 'JoinOrgDialog',
  props: {
    // 对话框可见性（v-model）
    modelValue: { type: Boolean, required: true },
    joining: { type: Boolean, required: true }
  },
  emits: ['update:modelValue', 'submit'],
  setup(props, { emit }) {
    const joinCode = ref('');

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    // 打开对话框时清空上次的邀请码（对齐原 openJoinDialog 行为）
    watch(
      () => props.modelValue,
      (visible) => {
        if (visible) {
          joinCode.value = '';
        }
      }
    );

    const onSubmit = () => {
      emit('submit', joinCode.value);
    };

    return {
      dialogVisible,
      joinCode,
      onSubmit
    };
  }
});
</script>
