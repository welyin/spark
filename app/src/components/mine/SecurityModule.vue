<!-- 安全设置模块（SettingsPage「个人设置」新增模块）：
     第三栏=安全设置项（修改密码 / N 天未使用自动锁定），第四栏=对应内容。
     device-trust-and-biometric 落地路线 §4（修改密码收紧为验当前密码）与
     §5（N 天未使用自动锁定，可选设置项，默认关闭）。
     修改密码必须验证当前密码，不提供任何生物识别/免密通道（§2 高危操作验密码）。 -->
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
    <!-- 修改密码 -->
    <el-card v-if="activeItem === 'password'" shadow="never" class="panel-card">
      <template #header>
        <h2>修改密码</h2>
      </template>
      <p class="hint">修改登录密码需先验证当前密码——这是保护账号的高危操作，不提供生物识别或免密通道。</p>
      <div class="security-form">
        <el-input
          v-model="oldPassword"
          type="password"
          show-password
          placeholder="当前密码"
          @keyup.enter="submitChangePassword"
        />
        <el-input
          v-model="newPassword"
          type="password"
          show-password
          placeholder="新密码（至少 8 位）"
          @keyup.enter="submitChangePassword"
        />
        <PasswordStrengthMeter :password="newPassword" />
        <el-input
          v-model="confirmPassword"
          type="password"
          show-password
          placeholder="确认新密码"
          @keyup.enter="submitChangePassword"
        />
        <div class="security-form-actions">
          <el-button type="primary" :loading="saving" :disabled="!canSubmit" @click="submitChangePassword">
            修改密码
          </el-button>
        </div>
        <el-alert v-if="formMessage" :title="formMessage" type="error" :closable="false" show-icon />
      </div>
    </el-card>

    <!-- N 天未使用自动锁定 -->
    <el-card v-else shadow="never" class="panel-card">
      <template #header>
        <h2>N 天未使用自动锁定</h2>
      </template>
      <!-- 文案与实现对齐：isAutoLockExpired 从 lastActiveAt（最近一次登录/解锁）起算
           超时，而非"真实未使用时长"——选低成本改文案（未引入活跃行为追踪）。 -->
      <p class="hint">距上次登录/解锁超过所选天数后，再次打开应用需重新输入密码。默认关闭（长期登录）。</p>
      <div class="security-form">
        <el-radio-group v-model="autoLockDays" @change="saveAutoLockDays">
          <el-radio v-for="opt in AUTO_LOCK_OPTIONS" :key="opt" :value="opt">
            {{ opt === 0 ? '关闭' : `${opt} 天` }}
          </el-radio>
        </el-radio-group>
        <p v-if="autoLockDays > 0" class="hint security-lock-tip">
          开启后，离开 {{ autoLockDays }} 天再次打开需重新输入密码，提升设备被他人拿到时的安全性。
        </p>
      </div>
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Key, Lock } from '@element-plus/icons-vue';
import { errorMessage } from '../../utils/ipc';
import { AUTO_LOCK_OPTIONS, getAutoLockDays, setAutoLockDays } from '../../utils/auto-lock';
import MineDetailContainer from './MineDetailContainer.vue';
import PasswordStrengthMeter from '../common/PasswordStrengthMeter.vue';

type SecurityItemKey = 'password' | 'autolock';

// color 为菜单图标色（微信式每项一色，同 MinePage 一级菜单色板）
const SECURITY_ITEMS: Array<{ key: SecurityItemKey; label: string; desc: string; icon: Component; color: string }> = [
  { key: 'password', label: '修改密码', desc: '需验证当前密码', icon: Key, color: '#3296fa' },
  { key: 'autolock', label: '自动锁定', desc: 'N 天未使用后需重新输密码', icon: Lock, color: '#ff7d00' }
];

export default defineComponent({
  name: 'SecurityModule',
  components: { MineDetailContainer, PasswordStrengthMeter },
  props: {
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    // drawer 模式初始无选中（抽屉关闭，只显示列表栏）；column 模式保持默认选中修改密码
    const activeItem = ref<SecurityItemKey | null>(props.detailMode === 'drawer' ? null : 'password');

    const activeItemLabel = computed(() => {
      const labels: Record<SecurityItemKey, string> = { password: '修改密码', autolock: '自动锁定' };
      return activeItem.value ? labels[activeItem.value] : '';
    });

    // ---------------- 修改密码 ----------------
    const oldPassword = ref('');
    const newPassword = ref('');
    const confirmPassword = ref('');
    const saving = ref(false);
    const formMessage = ref('');

    const canSubmit = computed(
      () => oldPassword.value.length > 0 && newPassword.value.length >= 8 && newPassword.value === confirmPassword.value
    );

    const submitChangePassword = async () => {
      if (saving.value) {
        return;
      }
      formMessage.value = '';
      if (!oldPassword.value) {
        formMessage.value = '请填写当前密码';
        return;
      }
      if (newPassword.value.length < 8) {
        formMessage.value = '新密码至少 8 位';
        return;
      }
      if (newPassword.value !== confirmPassword.value) {
        formMessage.value = '两次输入的新密码不一致';
        return;
      }
      saving.value = true;
      try {
        // 命令契约：root_change_password(oldPassword, newPassword) → { success: true }（对齐 dto.rs SuccessResult）
        await window.electronAPI.rootIdentity.changePassword(oldPassword.value, newPassword.value);
        // 改密成功后清空输入，密码已换，需用户记住新密码
        oldPassword.value = '';
        newPassword.value = '';
        confirmPassword.value = '';
        // 密码已换：既有的备份二维码/助记词仍按旧口令封存，需用户重新导出备份二维码
        ElMessage.success('密码已修改，建议重新导出备份二维码');
      } catch (error) {
        formMessage.value = `修改失败：${errorMessage(error)}`;
      } finally {
        saving.value = false;
      }
    };

    // ---------------- N 天未使用自动锁定 ----------------
    const autoLockDays = ref(getAutoLockDays());

    const saveAutoLockDays = () => {
      setAutoLockDays(autoLockDays.value);
      ElMessage.success(autoLockDays.value === 0 ? '已关闭自动锁定' : `已开启 ${autoLockDays.value} 天自动锁定`);
    };

    onMounted(() => {
      autoLockDays.value = getAutoLockDays();
    });

    return {
      securityItems: SECURITY_ITEMS,
      activeItem,
      activeItemLabel,
      oldPassword,
      newPassword,
      confirmPassword,
      saving,
      formMessage,
      canSubmit,
      submitChangePassword,
      AUTO_LOCK_OPTIONS,
      autoLockDays,
      saveAutoLockDays
    };
  }
});
</script>

<style scoped>
.security-form {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 14px;
  margin-top: 12px;
  max-width: 360px;
}

.security-form .el-input {
  width: 100%;
}

.security-form-actions {
  display: flex;
  gap: 12px;
}

.security-lock-tip {
  margin: 0;
}
</style>
