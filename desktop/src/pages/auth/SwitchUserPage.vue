<template>
  <section class="auth-panel">
    <h2 class="auth-title">切换用户</h2>
    <p class="hint">选择本设备上登录过的账号，或注册新用户。</p>

    <el-table :data="identities" v-loading="loading" empty-text="本设备还没有任何账号" class="identity-table">
      <el-table-column label="用户">
        <template #default="{ row }">
          <div class="user-cell">
            <UserAvatar :root-id="row.rootId" :nickname="row.nickname ?? ''" :avatar="row.avatar ?? ''" :size="32" />
            <span class="user-cell-name">{{ row.nickname || '未命名用户' }}</span>
          </div>
        </template>
      </el-table-column>
      <el-table-column label="创建时间" width="150">
        <template #default="{ row }">
          {{ formatTime(row.createdAt) }}
        </template>
      </el-table-column>
      <el-table-column width="130" fixed="right">
        <template #default="{ row }">
          <el-tag v-if="row.active" type="success" size="small">当前账号</el-tag>
          <el-button v-else type="primary" link @click="emit('select', row.rootId)">登录此账号</el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-button class="submit-btn" type="primary" @click="emit('register')">注册新用户</el-button>
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
  emits: ['select', 'register', 'back'],
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
