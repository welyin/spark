<template>
  <div class="security-panel">
    <p class="panel-desc">距上次登录/解锁超过所选天数后，再次打开应用需重新输入密码。默认关闭（长期登录）。</p>
    <div class="security-form">
      <el-radio-group v-model="autoLockDays" @change="save">
        <el-radio v-for="opt in AUTO_LOCK_OPTIONS" :key="opt" :value="opt">
          {{ opt === 0 ? '关闭' : `${opt} 天` }}
        </el-radio>
      </el-radio-group>
      <p v-if="autoLockDays > 0" class="hint">
        开启后，离开 {{ autoLockDays }} 天再次打开需重新输入密码，提升设备被他人拿到时的安全性。
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { AUTO_LOCK_OPTIONS, getAutoLockDays, setAutoLockDays } from '../../utils/auto-lock';

onMounted(() => {
  autoLockDays.value = getAutoLockDays();
});

const autoLockDays = ref(0);

function save() {
  setAutoLockDays(autoLockDays.value);
  ElMessage.success(autoLockDays.value === 0 ? '已关闭自动锁定' : `已开启 ${autoLockDays.value} 天自动锁定`);
}
</script>

<style scoped>
.security-panel {
  padding: 8px 0;
}
.panel-desc {
  margin: 0 0 16px;
  color: var(--spark-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}
.security-form {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.hint {
  margin: 0;
  color: var(--spark-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}
</style>
