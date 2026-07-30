<!-- 职责：添加成员对话框（与添加朋友同一套名片输入；预录入成员 -> 定向 DM 邀请对方确认） -->
<template>
  <el-dialog v-model="dialogVisible" title="添加成员" width="480px" @closed="resetInviteDialog">
    <!-- TODO(mock): 仅本地模式下邀请无法送达对方，前端先行拦截（真实实现应由内核/网关拒绝或排队） -->
    <el-alert
      v-if="isLocalOnly"
      title="当前为仅本地模式，无法发送邀请"
      type="warning"
      :closable="false"
      show-icon
      class="invite-local-alert"
    />
    <p class="hint">让对方打开「个人设置 → 我的名片」，把二维码名片图片或名片内容发给你。</p>
    <CardInput v-model="cardRaw" />
    <template #footer>
      <el-button @click="dialogVisible = false">取消</el-button>
      <el-button type="primary" :loading="inviting" @click="addMemberAndInvite">
        {{ inviting ? '处理中...' : '添加并发送邀请' }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import CardInput from '../common/CardInput.vue';
import { parseCard } from '../../utils/card';
import { applyOrgOutgoingInvite } from '../../mock/contacts';
import { useNetworkStatus } from '../../stores/network-status';

export default defineComponent({
  name: 'InviteMemberDialog',
  components: { CardInput },
  props: {
    // 对话框可见性（v-model）
    modelValue: { type: Boolean, required: true },
    orgId: { type: String, required: true },
    // 写操作前的网络检查（OrgPage 注入，只提示不阻断）
    beforeWrite: { type: Function as PropType<() => Promise<void>>, required: true },
    // 预录入成功后的回调（OrgPage 注入：刷新组织列表）
    onInvited: { type: Function as PropType<() => Promise<void>>, required: true }
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const cardRaw = ref('');
    const inviting = ref(false);
    const { isLocalOnly } = useNetworkStatus();

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const resetInviteDialog = () => {
      cardRaw.value = '';
    };

    // 打开对话框时重置表单（对齐原 openInviteDialog 行为）
    watch(
      () => props.modelValue,
      (visible) => {
        if (visible) {
          resetInviteDialog();
        }
      }
    );

    const addMemberAndInvite = async () => {
      if (!props.orgId) {
        return;
      }
      // TODO(mock): 仅本地拦截为前端演示逻辑；真实实现应由内核/网关层拒绝或排队
      if (isLocalOnly.value) {
        ElMessage.warning('仅本地模式下无法发送邀请');
        return;
      }
      const raw = cardRaw.value.trim();
      if (!raw) {
        ElMessage.warning('请上传名片图片或粘贴名片内容');
        return;
      }
      const card = parseCard(raw);
      if (!card) {
        ElMessage.warning('未从内容中识别到有效的身份 ID，请确认名片内容完整');
        return;
      }

      const nodeInfo = card.peerId || card.addresses?.length
        ? {
            peerId: card.peerId,
            addresses: card.addresses ?? []
          }
        : undefined;

      inviting.value = true;
      try {
        await props.beforeWrite();
        // 预录成员（保留）：记录 rootId + 名片寻址线索，成员列表即时可见
        await window.electronAPI.organization.addMember(props.orgId, {
          rootId: card.rootId,
          nodeInfo
        });
        // 定向 DM 邀请：名片寻址线索显式上送；发送成功后出站记录落库，
        // 「新的成员 → 我发出的邀请」经 inviteRecords 水合/OrgInviteUpdated 事件展示
        const invite = await window.electronAPI.organization.sendInvite({
          orgId: props.orgId,
          targetRootId: card.rootId,
          targetPeerId: card.peerId ?? null,
          targetAddresses: card.addresses ?? null
        });
        // 空间可能早已水合：即合入刚发出的记录，面板无需等下次水合/对方回执
        applyOrgOutgoingInvite(invite);
        ElMessage.success('邀请已发送，等待对方确认');
        dialogVisible.value = false;
        await props.onInvited();
      } catch (error) {
        // 内核错误原文提示（如「无法确定对方节点地址」）
        ElMessage.error(`${error}`);
      } finally {
        inviting.value = false;
      }
    };

    return {
      dialogVisible,
      cardRaw,
      inviting,
      isLocalOnly,
      resetInviteDialog,
      addMemberAndInvite
    };
  }
});
</script>

<style scoped>
.invite-local-alert {
  margin-bottom: 12px;
}
</style>
