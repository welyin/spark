<!-- 创建 / 加入组织面板（移植壳层 CreateOrgDialog/JoinOrgDialog 的表单语义，
     对话框形态改为面板内嵌表单）：personal 空间为唯一内容；org 空间作为
     「创建 / 加入组织」子项。创建/加入成功后的空间切换是壳层动作——桥无空间
     切换通道，插件提示用户手动切换（见任务报告缺口） -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>创建 / 加入组织</h2>
    </template>
    <div class="membership-grid">
      <section class="membership-block">
        <h3 class="section-title">创建组织</h3>
        <el-form label-position="top">
          <el-form-item label="组织 logo">
            <AvatarPicker v-model="createForm.avatar" :nickname="createForm.name" :size="48" />
            <p class="hint">可选；未上传时按组织自动生成首字配色头像。</p>
          </el-form-item>
          <el-form-item label="组织名称">
            <el-input v-model="createForm.name" placeholder="例如：产品组" maxlength="60" />
          </el-form-item>
          <el-form-item label="组织描述">
            <el-input v-model="createForm.description" type="textarea" :rows="3" placeholder="可选，描述组织用途" maxlength="200" />
          </el-form-item>
          <el-form-item label="域类型">
            <el-radio-group v-model="createForm.domainType">
              <el-radio value="leaf">普通组织</el-radio>
              <el-radio value="community">共同体域</el-radio>
            </el-radio-group>
            <p class="hint">共同体域的成员是组织（用于跨组织协作）；创建后不可变更。</p>
          </el-form-item>
        </el-form>
        <p class="hint">创建人会自动成为该组织的管理员和首位成员。</p>
        <el-button type="primary" :loading="busy" :disabled="!createForm.name.trim()" @click="createOrganization">
          {{ busy ? '创建中...' : '创建组织' }}
        </el-button>
      </section>

      <section class="membership-block">
        <h3 class="section-title">通过邀请码加入</h3>
        <el-input v-model="joinCode" type="textarea" :rows="5" placeholder="粘贴管理员分享给你的邀请码" />
        <p class="hint">加入前提：管理员已先将你的 RootID 录入组织成员。邀请码 24 小时内有效，用于连接管理员节点并拉取组织数据。</p>
        <el-button type="primary" :loading="busy" :disabled="!joinCode.trim()" @click="acceptInvite">
          {{ busy ? '加入中...' : '加入组织' }}
        </el-button>
      </section>
    </div>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { orgApi } from '../sdk-host';
import AvatarPicker from './AvatarPicker.vue';

export default defineComponent({
  name: 'OrgCreateJoinPanel',
  components: { AvatarPicker },
  emits: ['changed'],
  setup(_, { emit }) {
    const busy = ref(false);
    const createForm = ref<{ name: string; description: string; avatar: string; domainType: 'leaf' | 'community' }>({
      name: '',
      description: '',
      avatar: '',
      domainType: 'leaf'
    });
    const joinCode = ref('');

    const createOrganization = async () => {
      const api = orgApi();
      if (!api || busy.value) {
        return;
      }
      busy.value = true;
      try {
        const org = await api.create({ ...createForm.value });
        createForm.value = { name: '', description: '', avatar: '', domainType: 'leaf' };
        // 空间切换是壳层动作（桥无空间切换通道）：提示用户在壳层切到新组织空间
        ElMessage.success(`已创建「${org.name}」，请在空间列表切换到该组织空间`);
        emit('changed');
      } catch (error) {
        ElMessage.error(error instanceof Error ? error.message : '创建失败');
      } finally {
        busy.value = false;
      }
    };

    const acceptInvite = async () => {
      const api = orgApi();
      const code = joinCode.value.trim();
      if (!api || busy.value || !code) {
        return;
      }
      busy.value = true;
      try {
        const joined = await api.acceptInvite(code);
        joinCode.value = '';
        ElMessage.success(`已加入「${joined.orgName}」，请在空间列表切换到该组织空间`);
        emit('changed');
      } catch (error) {
        ElMessage.error(error instanceof Error ? error.message : '加入失败');
      } finally {
        busy.value = false;
      }
    };

    return { busy, createForm, joinCode, createOrganization, acceptInvite };
  }
});
</script>

<style scoped>
.panel-card h2 {
  margin: 0;
  font-size: 16px;
}

.section-title {
  margin: 0 0 8px;
  font-size: 14px;
}

.hint {
  color: var(--spark-text-2);
  font-size: 13px;
}

.membership-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 24px;
}
</style>
