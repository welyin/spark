<!-- 职责：创建组织对话框（logo + 名称 + 描述）；组织与插件不再强关联，不再选择基础插件 -->
<template>
  <el-dialog v-model="dialogVisible" title="创建组织" width="520px">
    <el-form label-position="top">
      <el-form-item label="组织 logo">
        <AvatarPicker v-model="createForm.avatar" :nickname="createForm.name" />
        <p class="hint">可选；未上传时按组织自动生成首字配色头像。</p>
      </el-form-item>
      <el-form-item label="组织名称">
        <el-input v-model="createForm.name" placeholder="例如：产品组" />
      </el-form-item>
      <el-form-item label="组织描述">
        <el-input
          v-model="createForm.description"
          type="textarea"
          :rows="3"
          placeholder="可选，描述组织用途"
        />
      </el-form-item>
    </el-form>
    <p class="hint">创建人会自动成为该组织的管理员和首位成员。</p>
    <template #footer>
      <el-button @click="dialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="creating" @click="onSubmit">
        {{ creating ? '创建中...' : '创建组织' }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import AvatarPicker from '../AvatarPicker.vue';
import type { CreateForm } from './types';

export default defineComponent({
  name: 'CreateOrgDialog',
  components: {
    AvatarPicker
  },
  props: {
    // 对话框可见性（v-model）
    modelValue: { type: Boolean, required: true },
    creating: { type: Boolean, required: true }
  },
  emits: ['update:modelValue', 'submit'],
  setup(props, { emit, expose }) {
    const createForm = ref<CreateForm>({ name: '', description: '', avatar: '' });

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const onSubmit = () => {
      emit('submit', { ...createForm.value });
    };

    /** 创建成功后由父组件调用：清空名称/描述/logo */
    const resetAfterCreate = () => {
      createForm.value = {
        name: '',
        description: '',
        avatar: ''
      };
    };

    expose({ resetAfterCreate });

    return {
      dialogVisible,
      createForm,
      onSubmit
    };
  }
});
</script>
