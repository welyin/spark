<!-- 安全设置模块（SettingsPage「个人设置」新增模块）：
     第三栏=安全设置项（修改密码 / 自动锁定 / 忘记密码重置），第四栏=对应内容。
     生物识别解锁已迁移到「系统设置」；M5 延迟恢复入口（仅移动端）仍保留在此。 -->
<template>
  <!-- 第三栏：安全设置项 -->
  <div class="mine-list">
    <h2 class="mine-list-title">安全设置</h2>
    <div class="mine-list-items">
      <button
        v-for="item in securityItems"
        :key="item.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeItem === item.key }"
        @click="activeItem = item.key"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="{ color: item.color }"
        ><component :is="item.icon" /></el-icon>
        <span class="mine-list-item-text">
          <b>{{ item.label }}</b>
          <span>{{ item.desc }}</span>
        </span>
      </button>
    </div>
  </div>

  <!-- 详情：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeItem !== null"
    :title="activeItemLabel"
    @close="activeItem = null"
  >
    <SecurityPasswordPanel
      v-if="activeItem === 'password'"
      @cancel="activeItem = null"
      @success="onPasswordChanged"
    />
    <SecurityAutoLockPanel
      v-else-if="activeItem === 'autolock'"
      @cancel="activeItem = null"
    />
    <SecurityRecoveryPanel
      v-else-if="activeItem === 'recovery'"
      :root-id="effectiveRootId"
      @cancel="activeItem = null"
    />
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Key, Lock, WarningFilled } from '@element-plus/icons-vue';
import { isMobileLayout } from '../../stores/ui-layout';
import { currentUser } from '../../stores/current-user';
import MineDetailContainer from './MineDetailContainer.vue';
import SecurityPasswordPanel from './SecurityPasswordPanel.vue';
import SecurityAutoLockPanel from './SecurityAutoLockPanel.vue';
import SecurityRecoveryPanel from './SecurityRecoveryPanel.vue';
import { inboundRecovery, pendingRecovery } from '../../stores/recovery';

export default defineComponent({
  name: 'SecurityModule',
  components: {
    MineDetailContainer,
    SecurityPasswordPanel,
    SecurityAutoLockPanel,
    SecurityRecoveryPanel
  },
  props: {
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' },
    /** 当前活动身份 rootId； SecurityModule 所在页面通常已持有 */
    rootId: { type: String, default: '' }
  },
  setup(props) {
    type Item = { key: 'password' | 'autolock' | 'recovery'; label: string; desc: string; icon: Component; color: string };

    const activeItem = ref<'password' | 'autolock' | 'recovery' | null>(props.detailMode === 'drawer' ? null : 'password');

    const effectiveRootId = computed(() => props.rootId || currentUser.rootId || '');

    const recoveryDesc = computed(() => {
      if (inboundRecovery.value) return `来自 ${inboundRecovery.value.fromDevice} 的恢复请求`;
      if (pendingRecovery.value) return '重置等待确认中';
      return '忘记密码时延迟重置';
    });

    const securityItems = computed<Item[]>(() => {
      const items: Item[] = [
        { key: 'password', label: '修改密码', desc: '需验证当前密码', icon: Key, color: '#3296fa' },
        { key: 'autolock', label: '自动锁定', desc: 'N 天未使用后需重新输密码', icon: Lock, color: '#ff7d00' }
      ];
      // 生物识别解锁已迁到「系统设置」，安全设置只保留 M5 延迟恢复入口
      if (isMobileLayout.value) {
        items.push({
          key: 'recovery',
          label: inboundRecovery.value ? '延迟恢复请求' : '忘记密码重置',
          desc: recoveryDesc.value,
          icon: WarningFilled,
          color: inboundRecovery.value ? '#f54a45' : '#f7b500'
        });
      }
      return items;
    });

    const activeItemLabel = computed(() => {
      const found = securityItems.value.find((i) => i.key === activeItem.value);
      return found?.label ?? '';
    });

    async function onPasswordChanged() {
      ElMessage.success('密码已修改，建议重新导出备份二维码');
      // 生物识别解锁迁到系统设置，改密后系统设置面板会自行保鲜；此处不再重复处理
    }

    return {
      securityItems,
      activeItem,
      activeItemLabel,
      effectiveRootId,
      onPasswordChanged
    };
  }
});
</script>

<style scoped>
.mine-list-item-text b {
  display: block;
}
</style>
