<template>
  <section class="auth-panel">
    <h2 class="auth-title">找回账号</h2>
    <p class="hint">选择恢复方式。</p>

    <div v-if="isMobileLayout" class="recover-tabs">
      <button
        v-for="tab in tabs"
        :key="tab.key"
        type="button"
        :class="['recover-tab', { active: activeTab === tab.key }]"
        @click="activeTab = tab.key"
      >{{ tab.label }}</button>
    </div>

    <template v-if="!isMobileLayout || activeTab === 'mnemonic'">
      <p class="hint">通过助记词恢复 RootID。</p>
      <el-steps :active="mnemonicStep" align-center finish-status="success" class="auth-steps">
        <el-step title="验证助记词" />
        <el-step title="设置资料与密码" />
      </el-steps>

      <template v-if="mnemonicStep === 0">
        <el-form label-position="top">
          <el-form-item label="助记词（24 个汉字或英文单词，汉字可空格分隔或连续书写）">
            <el-input
              v-model="mnemonicInput"
              type="textarea"
              :rows="3"
              placeholder="输入注册时记录的 24 个助记词"
              :disabled="busy"
            />
          </el-form-item>
        </el-form>
        <template v-if="checkWords.length > 0">
          <div class="mnemonic-grid">
            <span
              v-for="(word, index) in checkWords"
              :key="index"
              class="mnemonic-word"
              :class="{ invalid: invalidIndexes.includes(index) }"
            >
              <em>{{ index + 1 }}</em>
              {{ word }}
            </span>
          </div>
          <p class="hint">
            已识别 {{ checkWords.length }} / 24 个词<template v-if="invalidIndexes.length > 0">，红色为词表外错字</template>
          </p>
        </template>
        <el-button class="submit-btn" type="primary" :disabled="!mnemonicWordsValid" @click="mnemonicStep = 1">下一步</el-button>
      </template>

      <template v-else>
        <!-- 回车与点击统一走 submitMnemonic（形态与登录页一致）：@keydown.enter.prevent 显式触发，
             按钮 native-type="button" + @click；@submit.prevent 纯兜底防刷新 -->
        <el-form label-position="top" class="auth-form" @submit.prevent>
          <el-form-item label="昵称">
            <el-input v-model="nickname" placeholder="中英文均可，最长 24 个字符" maxlength="24" :disabled="busy" @keydown.enter.prevent="submitMnemonic" />
          </el-form-item>
          <el-form-item label="头像（可选）">
            <AvatarPicker v-model="avatarDataUrl" :nickname="nickname" :disabled="busy" />
          </el-form-item>
          <el-form-item label="新登录密码">
            <el-input v-model="newPassword" type="password" show-password placeholder="至少 8 位" :disabled="busy" @keydown.enter.prevent="submitMnemonic" />
            <PasswordStrengthMeter :password="newPassword" />
          </el-form-item>
          <el-form-item label="确认新密码">
            <el-input v-model="confirmPassword" type="password" show-password placeholder="重复输入新密码" :disabled="busy" @keydown.enter.prevent="submitMnemonic" />
          </el-form-item>
          <el-button class="submit-btn" type="primary" native-type="button" :loading="busy" :disabled="!mnemonicReady" @click="submitMnemonic">恢复账号</el-button>
        </el-form>
        <div class="entry-link">
          <el-button link type="info" :disabled="busy" @click="mnemonicStep = 0">上一步</el-button>
        </div>
        <p class="hint">助记词是账号最高权限：恢复无需旧密码，恢复后原设备密码不再适用。</p>
      </template>
    </template>

    <template v-if="isMobileLayout && activeTab === 'delay'">
      <div v-if="pendingRecovery" class="delay-status">
        <p class="delay-status-title">有进行中的延迟恢复请求</p>
        <p class="delay-status-row">
          <span>请求 ID</span>
          <span>{{ pendingRecovery.requestId }}</span>
        </p>
        <p class="delay-status-row">
          <span>状态</span>
          <span>{{ pendingRecovery.state === 'initiated' ? '公示中' : pendingRecovery.state === 'vetoed' ? '已否决' : '已执行' }}</span>
        </p>
        <p class="delay-status-row">
          <span>剩余时间</span>
          <span>{{ formatRemaining(remainingMs(pendingRecovery.deadline)) }}</span>
        </p>
        <p class="hint">公示期结束后，请解锁本机并在「设置 → 安全设置 → 忘记密码重置」中确认。</p>
      </div>
      <div v-else class="delay-status">
        <p class="delay-status-title">没有进行中的延迟恢复请求</p>
        <p class="hint">
          若你仍登录着一台可信手机，可在该手机上发起「忘记密码重置」；公示期结束后在本机确认即可重新设置密码。
        </p>
      </div>
    </template>

    <!-- 没有助记词的引导（PC 端）：去手机发起延迟恢复 / 死路说透（device-trust-and-biometric §2.2 PC 端口径） -->
    <div v-if="!isMobileLayout" class="recover-deadend">
      <p class="recover-deadend-title">没有助记词？</p>
      <p class="recover-deadend-desc">
        如果你还登录着一台手机，可以在手机上发起「延迟恢复」来重置密码。如果手机也丢了、又没有助记词，账号将无法恢复（去中心化没有服务器帮你找回），只能创建新账号。
      </p>
    </div>

    <div class="entry-link">
      <el-button link type="info" @click="emit('back')">{{ backLabel }}</el-button>
    </div>

    <el-alert v-if="message" :title="message" type="error" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import AvatarPicker from '../../components/AvatarPicker.vue';
import PasswordStrengthMeter from '../../components/common/PasswordStrengthMeter.vue';
import { errorMessage } from '../../utils/ipc';
import { isMobileLayout } from '../../stores/ui-layout';
import {
  formatRemaining,
  hydrateRecovery,
  pendingRecovery,
  remainingMs,
} from '../../stores/recovery';

export default defineComponent({
  name: 'RecoverPage',
  components: {
    AvatarPicker,
    PasswordStrengthMeter
  },
  props: {
    /** 返回按钮文案（由父级按返回目标传入，如"返回注册"/"返回用户列表"） */
    backLabel: {
      type: String,
      default: '返回注册'
    },
    /** 当前展示身份的 rootId，用于延迟恢复 tab 水合本地状态 */
    rootId: {
      type: String,
      default: ''
    }
  },
  emits: ['recovered', 'back'],
  setup(props, { emit }) {
    const busy = ref(false);
    const message = ref('');
    const activeTab = ref<'mnemonic' | 'delay'>('mnemonic');
    const tabs = [
      { key: 'mnemonic', label: '助记词' },
      { key: 'delay', label: '延迟恢复' }
    ] as const;

    onMounted(() => {
      if (props.rootId && isMobileLayout.value) {
        // 延迟恢复 tab 只读展示本地状态；水合失败时不阻塞助记词恢复流程
        hydrateRecovery(props.rootId).catch(() => {});
      }
    });

    // ---------------- 助记词恢复 ----------------
    const mnemonicStep = ref(0);
    const mnemonicInput = ref('');
    const checkWords = ref<string[]>([]);
    const invalidIndexes = ref<number[]>([]);
    const nickname = ref('');
    const avatarDataUrl = ref('');
    const newPassword = ref('');
    const confirmPassword = ref('');

    let checkTimer: ReturnType<typeof setTimeout> | null = null;
    watch(mnemonicInput, (value) => {
      if (checkTimer) {
        clearTimeout(checkTimer);
      }
      checkTimer = setTimeout(async () => {
        try {
          const result = await window.electronAPI.rootIdentity.checkMnemonic(value);
          checkWords.value = result.words;
          invalidIndexes.value = result.invalidIndexes;
        } catch {
          checkWords.value = [];
          invalidIndexes.value = [];
        }
      }, 300);
    });

    /** 第一步校验通过（24 个词且无错字）才可进入第二步 */
    const mnemonicWordsValid = computed(() => checkWords.value.length === 24 && invalidIndexes.value.length === 0);

    const mnemonicReady = computed(
      () =>
        mnemonicWordsValid.value &&
        nickname.value.trim().length > 0 &&
        newPassword.value.length >= 8 &&
        newPassword.value === confirmPassword.value
    );

    const submitMnemonic = async () => {
      // 回车提交不走按钮 disabled，需自查
      if (busy.value || !mnemonicReady.value) {
        return;
      }
      busy.value = true;
      message.value = '';
      try {
        const result = await window.electronAPI.rootIdentity.recoverMnemonic(
          mnemonicInput.value,
          newPassword.value,
          nickname.value.trim(),
          avatarDataUrl.value || null
        );
        emit('recovered', result.rootId);
      } catch (error) {
        message.value = `恢复失败：${errorMessage(error)}`;
      } finally {
        busy.value = false;
      }
    };

    return {
      isMobileLayout,
      tabs,
      activeTab,
      pendingRecovery,
      remainingMs,
      formatRemaining,
      busy,
      message,
      mnemonicInput,
      checkWords,
      invalidIndexes,
      mnemonicStep,
      mnemonicWordsValid,
      nickname,
      avatarDataUrl,
      newPassword,
      confirmPassword,
      mnemonicReady,
      submitMnemonic,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/recover.css"></style>

<style scoped>
.recover-tabs {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
  border-bottom: 1px solid var(--spark-border-light);
  padding-bottom: 8px;
}
.recover-tab {
  flex: 1;
  padding: 8px 0;
  border: none;
  background: transparent;
  color: var(--spark-text-secondary);
  font-size: 14px;
  cursor: pointer;
  border-radius: 6px;
  transition: background 0.2s, color 0.2s;
}
.recover-tab.active {
  background: var(--spark-primary-bg);
  color: var(--spark-primary);
  font-weight: 500;
}
.delay-status {
  margin-top: 8px;
}
.delay-status-title {
  margin: 0 0 12px;
  font-size: 15px;
  font-weight: 500;
  color: var(--spark-text-primary);
}
.delay-status-row {
  display: flex;
  justify-content: space-between;
  margin: 6px 0;
  font-size: 14px;
  color: var(--spark-text-secondary);
}
.delay-status-row span:last-child {
  color: var(--spark-text-primary);
  font-weight: 500;
}
</style>
