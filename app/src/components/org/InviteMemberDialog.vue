<!-- 职责：添加成员对话框（与添加朋友同一套名片输入；预录入成员 -> 生成邀请码） -->
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
    <template v-if="!inviteResult">
      <p class="hint">让对方打开「个人设置 → 我的名片」，把二维码名片图片或名片内容发给你。</p>
      <CardInput v-model="cardRaw" />
    </template>
    <template v-else>
      <el-alert title="成员已预录入，邀请码已生成" type="success" :closable="false" show-icon />
      <el-form label-position="top" class="invite-result">
        <el-form-item label="邀请码（24 小时内有效）">
          <el-input v-model="inviteResult" type="textarea" :rows="4" readonly />
        </el-form-item>
      </el-form>
      <p class="hint">请通过线下渠道（微信/当面）把邀请码发给对方；对方凭码连接你的节点完成加入，期间你需要保持在线。</p>
    </template>
    <template #footer>
      <template v-if="!inviteResult">
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="inviting" @click="addMemberAndInvite">
          {{ inviting ? '处理中...' : '添加并生成邀请码' }}
        </el-button>
      </template>
      <template v-else>
        <el-button @click="dialogVisible = false">完成</el-button>
        <el-button type="primary" @click="copyInvite">复制邀请码</el-button>
      </template>
    </template>
  </el-dialog>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import CardInput from '../common/CardInput.vue';
import { parseCard } from '../../utils/card';
import { recordOutgoing, spaceKeyOf } from '../../mock/contacts';
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
    const inviteResult = ref('');
    const inviting = ref(false);
    const { isLocalOnly } = useNetworkStatus();

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const resetInviteDialog = () => {
      cardRaw.value = '';
      inviteResult.value = '';
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
        await window.electronAPI.organization.addMember(props.orgId, {
          rootId: card.rootId,
          nodeInfo
        });
        const invite = await window.electronAPI.organization.createInvite(props.orgId);
        inviteResult.value = invite.invite;
        // 「新的成员」里留一条我发出的邀请记录，可看到对方反应/再复制邀请码
        recordOutgoing(spaceKeyOf({ type: 'org', orgId: props.orgId }), {
          rootId: card.rootId,
          source: '名片',
          inviteCode: invite.invite
        });
        ElMessage.success('成员已预录入');
        await props.onInvited();
      } catch (error) {
        ElMessage.error(`邀请成员失败：${error}`);
      } finally {
        inviting.value = false;
      }
    };

    const copyInvite = async () => {
      try {
        await navigator.clipboard.writeText(inviteResult.value);
        ElMessage.success('邀请码已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    return {
      dialogVisible,
      cardRaw,
      inviteResult,
      inviting,
      isLocalOnly,
      resetInviteDialog,
      addMemberAndInvite,
      copyInvite
    };
  }
});
</script>

<style scoped>
.invite-local-alert {
  margin-bottom: 12px;
}

.invite-result {
  margin-top: 12px;
}
</style>
