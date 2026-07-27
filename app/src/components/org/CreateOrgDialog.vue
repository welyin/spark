<!-- 职责：创建组织对话框（名称 + 基础插件 + 描述） -->
<template>
  <el-dialog v-model="dialogVisible" title="创建组织" width="520px">
    <el-form label-position="top">
      <el-form-item label="组织名称">
        <el-input v-model="createForm.name" placeholder="例如：产品组" />
      </el-form-item>
      <el-form-item label="基础插件">
        <el-select v-model="createForm.basePluginDomain" placeholder="请选择组织基础插件" style="width: 100%">
          <el-option
            v-for="plugin in foundationPlugins"
            :key="plugin.domain"
            :label="`${plugin.name} (${plugin.domain})`"
            :value="plugin.domain"
          />
        </el-select>
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
    <p class="hint">创建人会自动成为该组织的管理员和首位成员。组织必须绑定一个基础插件。</p>
    <template #footer>
      <el-button @click="dialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="creating" @click="onSubmit">
        {{ creating ? '创建中...' : '创建组织' }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import type { CreateForm, PluginCatalogItem } from './types';

export default defineComponent({
  name: 'CreateOrgDialog',
  props: {
    // 对话框可见性（v-model）
    modelValue: { type: Boolean, required: true },
    creating: { type: Boolean, required: true },
    foundationPlugins: { type: Array as PropType<PluginCatalogItem[]>, required: true }
  },
  emits: ['update:modelValue', 'submit'],
  setup(props, { emit, expose }) {
    const createForm = ref<CreateForm>({ name: '', description: '', basePluginDomain: '' });

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    // 插件目录就绪后缺省选中第一个基础插件（对齐原 loadPluginCatalog 行为）
    watch(
      () => props.foundationPlugins,
      (plugins) => {
        if (!createForm.value.basePluginDomain) {
          createForm.value.basePluginDomain = plugins[0]?.domain ?? '';
        }
      }
    );

    const onSubmit = () => {
      emit('submit', { ...createForm.value });
    };

    /** 创建成功后由 OrgPage 调用：清空名称/描述，保留已选基础插件 */
    const resetAfterCreate = () => {
      createForm.value = {
        name: '',
        description: '',
        basePluginDomain: createForm.value.basePluginDomain
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
