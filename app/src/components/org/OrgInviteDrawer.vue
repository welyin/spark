<!-- 组织邀请确认抽屉（系统会话邀请卡片点击弹出）：
     组织资料卡（头像/名称/描述/邀请人行）+ 状态区（pending=确认加入/拒绝按钮，
     accepted=「已加入」，declined=「已拒绝」）。打开时按 orgId 调 inviteRecords
     取该 inviteId 对应 incoming 记录的最新状态；确认/拒绝走 respondInvite（内核幂等，
     accept 先执行加入编排，成功才落 accepted）。抽屉壳与 ContactCardDrawer 同款。 -->
<template>
  <el-drawer :model-value="modelValue" :with-header="false" size="440" class="app-drawer" @update:model-value="setVisible">
    <button type="button" class="app-drawer-close" title="关闭" @click="setVisible(false)">
      <el-icon :size="16"><Close /></el-icon>
    </button>
    <div class="app-drawer-body">
      <div class="invite-hero">
        <OrgAvatar :org-id="orgId" :name="orgName" :avatar="record?.orgAvatar" :size="64" />
        <h2 class="invite-name">{{ orgName }}</h2>
        <p class="invite-by">{{ inviterText }}</p>
      </div>

      <div v-if="orgDescription" class="invite-rows">
        <div class="info-row">
          <span class="info-label">组织简介</span>
          <span>{{ orgDescription }}</span>
        </div>
      </div>

      <!-- 状态区：记录加载失败（非 Tauri / 记录不存在）时无按钮 -->
      <div v-if="record" class="invite-actions">
        <template v-if="record.status === 'pending'">
          <el-button type="primary" :loading="acting === 'accept'" :disabled="acting !== ''" @click="respond(true)">
            确认加入
          </el-button>
          <el-button :loading="acting === 'decline'" :disabled="acting !== ''" @click="respond(false)">
            拒绝
          </el-button>
        </template>
        <p v-else-if="record.status === 'accepted'" class="invite-done">已加入</p>
        <p v-else class="invite-done">已拒绝</p>
      </div>
      <el-empty v-else-if="!loading" :image-size="110" description="无法获取邀请详情" />
    </div>
  </el-drawer>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { Close } from '@element-plus/icons-vue';
import OrgAvatar from '../OrgAvatar.vue';
import { findOrg, refreshOrganizations } from '../../stores/org-membership';
import type { OrgInviteRecordDto } from '../../api';

export default defineComponent({
  name: 'OrgInviteDrawer',
  components: { OrgAvatar, Close },
  props: {
    modelValue: { type: Boolean, required: true },
    /** 邀请 id（spark-org-invite://{inviteId} 链接路径段） */
    inviteId: { type: String, required: true },
    /** 组织 id（链接卡片 link.domain） */
    orgId: { type: String, required: true },
    /** 记录拉取失败时的兜底展示（链接卡片 title=组织名） */
    fallbackTitle: { type: String, default: '' },
    /** 记录拉取失败时的兜底展示（链接卡片 description="{邀请人昵称} 正在邀请你加入"） */
    fallbackDescription: { type: String, default: '' }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const setVisible = (visible: boolean) => emit('update:modelValue', visible);

    /** 当前邀请的 incoming 记录（打开抽屉时按 orgId 拉取最新状态） */
    const record = ref<OrgInviteRecordDto | null>(null);
    const loading = ref(false);
    /** 进行中的动作（'accept'/'decline'；'' = 空闲），防重复点击 */
    const acting = ref<'' | 'accept' | 'decline'>('');

    const orgName = computed(() => record.value?.orgName || props.fallbackTitle || '组织邀请');
    const inviterText = computed(() => {
      if (record.value) {
        return `${record.value.peerNickname} 正在邀请你加入`;
      }
      return props.fallbackDescription;
    });
    /** 组织简介：邀请记录不带描述，仅本机已缓存该组织（如已是成员）时可展示 */
    const orgDescription = computed(() => findOrg(props.orgId)?.description ?? '');

    async function load() {
      loading.value = true;
      record.value = null;
      try {
        const records = await window.electronAPI.organization.inviteRecords(props.orgId);
        record.value =
          records.find((item) => item.id === props.inviteId && item.direction === 'incoming') ?? null;
      } catch {
        record.value = null;
      } finally {
        loading.value = false;
      }
    }

    // 打开抽屉即拉取最新状态（对方回执/本机其他端处理过都能反映）
    watch(
      () => props.modelValue,
      (visible) => {
        if (visible) {
          void load();
        }
      }
    );

    async function respond(accept: boolean) {
      if (acting.value !== '') {
        return;
      }
      acting.value = accept ? 'accept' : 'decline';
      try {
        record.value = await window.electronAPI.organization.respondInvite({
          inviteId: props.inviteId,
          accept
        });
        if (accept) {
          ElMessage.success('已加入组织');
          refreshOrganizations().catch(() => {});
        } else {
          ElMessage.success('已拒绝');
        }
      } catch (error) {
        // 失败不动状态（内核保证 accept 编排失败不落 accepted），错误原文提示
        ElMessage.error(`${error}`);
      } finally {
        acting.value = '';
      }
    }

    return {
      record,
      loading,
      acting,
      orgName,
      inviterText,
      orgDescription,
      setVisible,
      respond
    };
  }
});
</script>

<style scoped>
.invite-hero {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding-top: 24px;
}

.invite-name {
  margin: 0;
  font-size: 18px;
  color: var(--spark-text-1);
}

.invite-by {
  margin: 0;
  font-size: 13px;
  color: var(--spark-text-2);
}

.invite-rows {
  margin-top: 20px;
}

.info-row {
  display: flex;
  gap: 12px;
  font-size: 13px;
  line-height: 1.6;
  color: var(--spark-text-1);
}

.info-label {
  flex-shrink: 0;
  color: var(--spark-text-2);
}

.invite-actions {
  display: flex;
  justify-content: center;
  gap: 12px;
  margin-top: 28px;
}

.invite-done {
  margin: 0;
  font-size: 14px;
  color: var(--spark-text-2);
}
</style>
