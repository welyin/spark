<!-- 个人空间「空间设置」对话框（走查决策：个人空间允许设置名字和 logo）：
     入口＝顶栏「当前空间」菜单的「空间设置」（个人空间不再置灰；组织空间同位为
     SpaceSettingsDialog/OrgSettingsPanel）。数据存本机（stores/personal-space），
     不同步不跨端。弹法同 L12 壳层模态（顶级对话框＋遮罩）。 -->
<template>
  <el-dialog
    v-model="visible"
    title="空间设置 · 个人空间"
    width="min(420px, 92vw)"
  >
    <div class="personal-space-settings">
      <div class="personal-space-logo">
        <AvatarPicker v-model="logo" :nickname="name || DEFAULT_PERSONAL_SPACE_NAME" :size="64" />
        <p class="personal-space-hint">空间 logo，仅本机生效；缺省使用你的根身份头像。</p>
      </div>
      <el-input
        v-model="name"
        maxlength="24"
        show-word-limit
        :placeholder="DEFAULT_PERSONAL_SPACE_NAME"
        clearable
      >
        <template #prepend>空间名</template>
      </el-input>
      <p class="personal-space-hint">留空则恢复缺省名「个人空间」。名字与 logo 只保存在这台设备上。</p>
    </div>
    <template #footer>
      <el-button @click="visible = false">取消</el-button>
      <el-button type="primary" @click="save">保存</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import AvatarPicker from '../AvatarPicker.vue';
import {
  DEFAULT_PERSONAL_SPACE_NAME,
  personalSpaceLogo,
  personalSpaceName,
  savePersonalSpaceProfile
} from '../../stores/personal-space';

export default defineComponent({
  name: 'PersonalSpaceSettingsDialog',
  components: { AvatarPicker },
  props: {
    modelValue: { type: Boolean, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const visible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const name = ref('');
    const logo = ref('');
    // 每次打开回填当前资料（显示去缺省化后的原名，占位符提示缺省名）
    watch(
      () => props.modelValue,
      (open) => {
        if (open) {
          name.value = personalSpaceName.value === DEFAULT_PERSONAL_SPACE_NAME ? '' : personalSpaceName.value;
          logo.value = personalSpaceLogo.value;
        }
      },
      { immediate: true }
    );

    const save = () => {
      savePersonalSpaceProfile(name.value, logo.value);
      visible.value = false;
      ElMessage.success('个人空间已更新');
    };

    return { visible, name, logo, save, DEFAULT_PERSONAL_SPACE_NAME };
  }
});
</script>

<style scoped>
.personal-space-settings {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.personal-space-logo {
  display: flex;
  align-items: center;
  gap: 14px;
}

.personal-space-hint {
  margin: 0;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  line-height: 1.5;
}
</style>
