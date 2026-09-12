<!-- 跨域投递确认框（problem X1/X2）：源对象与目标窗口/落点分属不同域时，松手弹出。
     明示「将把 X 域对象发到 Y 域」、可取消；可勾「本次会话、同一 源→目标 不再提示」——
     豁免只活在 stores/cross-domain-drop 内存集（关会话即失效），无永久豁免。 -->
<template>
  <el-dialog
    :model-value="!!pending"
    title="跨空间投递确认"
    width="min(440px, 92vw)"
    :close-on-click-modal="false"
    @update:model-value="onVisibleChange"
    @closed="remember = false"
  >
    <template v-if="pending">
      <p class="xdrop-line">
        将把「{{ sourceLabel }}」的对象
        <strong>{{ pending.object.label }}</strong>
        发到「{{ targetLabel }}」。
      </p>
      <p class="xdrop-scope">
        <el-icon :size="14"><WarningFilled /></el-icon>
        {{ scopeLabel }}
      </p>
      <el-checkbox v-model="remember" class="xdrop-remember">
        本次会话，同一 源→目标 不再提示（关闭应用即失效）
      </el-checkbox>
    </template>
    <template #footer>
      <el-button @click="cancel">取消</el-button>
      <el-button type="primary" @click="confirm">投递到{{ targetLabel }}</el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { WarningFilled } from '@element-plus/icons-vue';
import {
  cancelCrossDomainDrop,
  confirmCrossDomainDrop,
  pendingCrossDomainDrop,
  scopeLabelOf,
  spaceLabelOf
} from '../../stores/cross-domain-drop';

export default defineComponent({
  name: 'CrossDomainDropConfirm',
  components: { WarningFilled },
  setup() {
    const pending = pendingCrossDomainDrop;
    const remember = ref(false);
    const sourceLabel = computed(() => (pending.value ? spaceLabelOf(pending.value.object.source) : ''));
    const targetLabel = computed(() => (pending.value ? spaceLabelOf(pending.value.target) : ''));
    const scopeLabel = computed(() => (pending.value ? scopeLabelOf(pending.value.target) : ''));

    const confirm = () => {
      confirmCrossDomainDrop(remember.value);
      remember.value = false;
    };
    const cancel = () => {
      cancelCrossDomainDrop();
      remember.value = false;
    };
    // 对话框被点击遮罩/Esc 之外的途径关闭时按取消处理（close-on-click-modal 已禁，仅剩右上角 ×）
    const onVisibleChange = (visible: boolean) => {
      if (!visible) cancel();
    };

    return { pending, remember, sourceLabel, targetLabel, scopeLabel, confirm, cancel, onVisibleChange };
  }
});
</script>

<style scoped>
.xdrop-line {
  margin: 0 0 8px;
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
  line-height: 1.6;
}

.xdrop-scope {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0 0 12px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-warning, var(--el-color-warning));
}

.xdrop-remember {
  font-size: var(--spark-font-size-secondary);
}
</style>
