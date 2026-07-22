<template>
  <section class="auth-panel">
    <h2 class="auth-title">用户注册</h2>
    <p class="hint">首次注册将创建 RootID（仅本地存储）。</p>

    <template v-if="step === 'password'">
      <el-form label-position="top" class="auth-form">
        <el-form-item label="昵称">
          <el-input v-model="nickname" placeholder="中英文均可，最长 24 个字符" maxlength="24" :disabled="busy" />
        </el-form-item>
        <el-form-item label="头像（可选）">
          <AvatarPicker v-model="avatarDataUrl" :nickname="nickname" :disabled="busy" />
          <p class="hint">不上传时将按账号自动生成配色头像，之后可在"我的"页随时更换。</p>
        </el-form-item>
        <el-form-item label="登录密码">
          <el-input v-model="password" type="password" show-password placeholder="至少 8 位" :disabled="busy" />
        </el-form-item>
        <el-form-item label="确认密码">
          <el-input v-model="confirmPassword" type="password" show-password placeholder="重复输入密码" :disabled="busy" />
        </el-form-item>
      </el-form>
      <el-button class="submit-btn" type="primary" :loading="busy" :disabled="busy" @click="submit">创建 RootID</el-button>
      <div class="entry-link">
        <el-button link type="primary" @click="emit('recover')">已有助记词或备份二维码？恢复账号</el-button>
      </div>
    </template>

    <template v-else>
      <el-alert
        title="请离线抄写并妥善保存这 24 个汉字（顺序重要）。它是账号的最终兜底：不要截图、拍照或通过网络发送。"
        type="warning"
        :closable="false"
        show-icon
        class="auth-form"
      />
      <div class="mnemonic-grid">
        <span v-for="(word, index) in mnemonicWords" :key="index" class="mnemonic-word">
          <em>{{ index + 1 }}</em>
          {{ word }}
        </span>
      </div>
      <el-button class="submit-btn" type="primary" @click="copyMnemonic">复制助记词</el-button>
      <div class="entry-link">
        <el-button link type="primary" :disabled="!copied" @click="finish">已保存，进入应用</el-button>
        <el-button link type="info" @click="finish">稍后备份，先进入应用</el-button>
      </div>
      <el-alert
        v-if="copied"
        title="已复制。请粘贴到安全的离线位置（如密码管理器），或按顺序抄写纸质保存。"
        type="success"
        :closable="false"
        show-icon
        class="block-gap"
      />
    </template>

    <el-alert v-if="message" :title="message" type="error" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import AvatarPicker from '../../components/AvatarPicker.vue';
import { markIdentityBackupDone } from '../../utils/backup-state';
import { errorMessage } from '../../utils/ipc';

export default defineComponent({
  name: 'RegisterPage',
  components: {
    AvatarPicker
  },
  emits: ['registered', 'recover'],
  setup(_, { emit }) {
    const step = ref<'password' | 'mnemonic'>('password');
    const nickname = ref('');
    const avatarDataUrl = ref('');
    const password = ref('');
    const confirmPassword = ref('');
    const busy = ref(false);
    const message = ref('');
    const mnemonic = ref('');
    const rootId = ref('');
    const copied = ref(false);

    const mnemonicWords = computed(() => (mnemonic.value ? mnemonic.value.split(' ') : []));

    const submit = async () => {
      if (!nickname.value.trim()) {
        message.value = '请先填写昵称';
        return;
      }
      if (password.value.length < 8) {
        message.value = '密码至少 8 位';
        return;
      }
      if (password.value !== confirmPassword.value) {
        message.value = '两次密码不一致';
        return;
      }

      busy.value = true;
      message.value = '';
      try {
        const result = await window.electronAPI.rootIdentity.initialize(
          password.value,
          nickname.value.trim(),
          avatarDataUrl.value || null
        );
        mnemonic.value = result.mnemonic;
        rootId.value = result.rootId;
        step.value = 'mnemonic';
      } catch (error) {
        message.value = `注册失败：${errorMessage(error)}`;
      } finally {
        busy.value = false;
      }
    };

    const copyMnemonic = async () => {
      try {
        await navigator.clipboard.writeText(mnemonic.value);
        copied.value = true;
        markIdentityBackupDone(rootId.value);
        ElMessage.success('已复制助记词');
      } catch {
        ElMessage.warning('复制失败，请手动抄写助记词');
      }
    };

    const finish = () => {
      emit('registered', rootId.value);
    };

    return {
      step,
      nickname,
      avatarDataUrl,
      password,
      confirmPassword,
      busy,
      message,
      mnemonicWords,
      copied,
      submit,
      copyMnemonic,
      finish,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/register.css"></style>
