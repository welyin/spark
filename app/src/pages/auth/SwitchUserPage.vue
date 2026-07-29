<template>
  <section class="auth-panel">
    <h2 class="auth-title">切换用户</h2>
    <p class="hint">选择本设备上登录过的账号，或注册新用户。</p>

    <div v-loading="loading" class="identity-list">
      <p v-if="!loading && identities.length === 0" class="identity-empty">本设备还没有任何账号</p>
      <div
        v-for="row in identities"
        :key="row.rootId"
        class="identity-item"
        :class="{ clickable: !row.active }"
        @click="!row.active && emit('select', row.rootId)"
      >
        <UserAvatar :root-id="row.rootId" :nickname="row.nickname ?? ''" :avatar="row.avatar ?? ''" :size="40" />
        <div class="identity-info">
          <span class="identity-name">{{ row.nickname || '未命名用户' }}</span>
          <span class="identity-time">创建于 {{ formatTime(row.createdAt) }}</span>
        </div>
        <el-tag v-if="row.active" type="primary" size="small" class="identity-tag">当前账号</el-tag>
        <el-button v-else type="primary" link @click.stop="emit('select', row.rootId)">登录此账号</el-button>
      </div>
    </div>

    <div class="btn-row">
      <el-button class="btn-row-item" type="primary" @click="emit('register')">注册新用户</el-button>
      <el-button class="btn-row-item" plain @click="emit('recover')">添加其它账号</el-button>
    </div>
    <div class="entry-link">
      <el-button link type="info" @click="emit('back')">返回登录</el-button>
    </div>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref } from 'vue';
import UserAvatar from '../../components/UserAvatar.vue';

type IdentityItem = {
  rootId: string;
  createdAt: number;
  active: boolean;
  nickname: string | null;
  avatar: string | null;
};

export default defineComponent({
  name: 'SwitchUserPage',
  components: {
    UserAvatar
  },
  emits: ['select', 'register', 'recover', 'back'],
  setup(_, { emit }) {
    const identities = ref<IdentityItem[]>([]);
    const loading = ref(false);

    const formatTime = (ts: number) => {
      if (!ts) {
        return '-';
      }
      return new Date(ts).toLocaleString();
    };

    onMounted(async () => {
      loading.value = true;
      try {
        identities.value = await window.electronAPI.rootIdentity.listIdentities();
      } finally {
        loading.value = false;
      }
    });

    return {
      identities,
      loading,
      formatTime,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/switch-user.css"></style>
