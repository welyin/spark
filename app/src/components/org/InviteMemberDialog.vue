<!-- 职责：邀请成员对话框（两步：预录入成员 RootID -> 生成邀请码） -->
<template>
  <el-dialog v-model="dialogVisible" title="邀请成员" width="520px" @closed="resetInviteDialog">
    <template v-if="!inviteResult">
      <el-form label-position="top">
        <el-form-item label="成员 RootID">
          <el-input v-model="inviteRootId" placeholder="64 位 RootID" />
        </el-form-item>
        <el-collapse class="invite-advanced">
          <el-collapse-item title="高级选项：对方节点信息（可选）" name="advanced">
            <el-form-item label="成员 PeerId（可选）">
              <el-input v-model="invitePeerId" placeholder="例如：12D3KooW..." />
            </el-form-item>
            <el-form-item label="成员节点地址（可选，可多条，逗号/分号/换行分隔）">
              <el-input
                v-model="inviteAddresses"
                type="textarea"
                :rows="3"
                placeholder="例如：/ip4/127.0.0.1/tcp/15002/ws"
              />
            </el-form-item>
          </el-collapse-item>
        </el-collapse>
      </el-form>
      <p class="hint">只填 RootID 即可预录入；对方凭邀请码加入时会自动回填节点地址。</p>
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

export default defineComponent({
  name: 'InviteMemberDialog',
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
    const inviteRootId = ref('');
    const invitePeerId = ref('');
    const inviteAddresses = ref('');
    const inviteResult = ref('');
    const inviting = ref(false);

    const dialogVisible = computed({
      get: () => props.modelValue,
      set: (value: boolean) => emit('update:modelValue', value)
    });

    const resetInviteDialog = () => {
      inviteRootId.value = '';
      invitePeerId.value = '';
      inviteAddresses.value = '';
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
      if (!inviteRootId.value.trim()) {
        ElMessage.warning('请输入成员 RootID');
        return;
      }

      const addresses = inviteAddresses.value
        .split(/\r?\n|,|;/)
        .map((item) => item.trim())
        .filter((item) => item.length > 0);
      const nodeInfo = invitePeerId.value.trim() || addresses.length > 0
        ? {
            peerId: invitePeerId.value.trim() || undefined,
            addresses
          }
        : undefined;

      inviting.value = true;
      try {
        await props.beforeWrite();
        await window.electronAPI.organization.addMember(props.orgId, {
          rootId: inviteRootId.value,
          nodeInfo
        });
        const invite = await window.electronAPI.organization.createInvite(props.orgId);
        inviteResult.value = invite.invite;
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
      inviteRootId,
      invitePeerId,
      inviteAddresses,
      inviteResult,
      inviting,
      resetInviteDialog,
      addMemberAndInvite,
      copyInvite
    };
  }
});
</script>
