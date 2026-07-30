<!-- 新的朋友/新的成员（通讯录第三、四栏，ui-contacts §2.2）：
     第三栏=收到的申请与我发出的混排列表（按时间倒序，行首箭头区分收/发，右上角可筛选）；
     点击行在第四栏展示详情。收到的申请：接受前选择「向其开放的权限」（§6，仅个人空间），
     已接受（个人空间）直接内嵌联系人资料卡（可编辑备注等）；我发出的：展示对方反应（等待确认/已回复询问/已拒绝/已接受/
     连接失败可重试），组织邀请可复制邀请码。与个人设置模块同构，以 fragment 渲染两栏 -->
<template>
  <!-- 第三栏：申请列表（收发混排，按时间倒序） -->
  <div class="contacts-request-list">
    <div class="request-list-header">
      <h2 class="contacts-request-title">{{ spaceType === 'org' ? '新的成员' : '新的朋友' }}</h2>
      <el-dropdown trigger="click" @command="onFilterCommand">
        <span class="request-filter">
          {{ filterLabel }}<el-icon :size="12"><ArrowDown /></el-icon>
        </span>
        <template #dropdown>
          <el-dropdown-menu>
            <el-dropdown-item command="all">全部</el-dropdown-item>
            <el-dropdown-item command="in">收到的申请</el-dropdown-item>
            <el-dropdown-item command="out">{{ spaceType === 'org' ? '我发出的邀请' : '我发出的' }}</el-dropdown-item>
          </el-dropdown-menu>
        </template>
      </el-dropdown>
    </div>
    <el-empty v-if="entries.length === 0" description="暂无申请" />
    <button
      v-for="entry in entries"
      :key="rowKey(entry.dir, entry.request.id)"
      type="button"
      class="request-item"
      :class="{ active: activeKey === rowKey(entry.dir, entry.request.id) }"
      @click="selectRequest(entry.dir, entry.request.id)"
    >
      <!-- 方向标识：左下箭头=收到（绿），右上箭头=发出（蓝） -->
      <el-icon
        :size="13"
        class="dir-icon"
        :class="entry.dir"
        :title="entry.dir === 'in' ? '收到的申请' : '我发出的'"
      >
        <BottomLeft v-if="entry.dir === 'in'" />
        <TopRight v-else />
      </el-icon>
      <!-- 有未读新变化时头像右上角红点 -->
      <span class="request-avatar" :class="{ unread: entry.request.unread }">
        <UserAvatar :root-id="entry.request.rootId" :nickname="entry.request.nickname" :avatar="personImage(entry.request)" :size="40" />
      </span>
      <span class="request-item-main">
        <b>{{ personName(entry.request) }}</b>
        <span>{{ rowSubtitle(entry.dir, entry.request) }}</span>
      </span>
      <el-tag
        v-if="entry.request.status !== 'pending'"
        size="small"
        :type="statusTagType(entry.request.status)"
      >
        {{ statusLabel(entry.dir, entry.request.status) }}
      </el-tag>
    </button>
  </div>

  <!-- 第四栏：已接受（个人空间）直接展示联系人资料卡（可编辑备注等）；
       其余为选中申请详情 -->
  <div class="contacts-detail">
    <ContactPanel
      v-if="acceptedContact && acceptedProfile"
      :key="acceptedContact.rootId"
      :contact="acceptedContact"
      space-type="personal"
      :profile="acceptedProfile"
      :all-tags="tags"
      :group-options="groupOptions"
      :on-create-tag="onCreateTag"
      @save-profile="onSaveProfile"
      @set-blocked="onSetBlocked"
      @delete="onDeleteFriend"
      @send-message="onSendMessage"
    />
    <div v-else-if="activeEntry" class="request-profile">
      <div class="contact-panel-hero">
        <UserAvatar :root-id="activeEntry.request.rootId" :nickname="personName(activeEntry.request)" :avatar="personImage(activeEntry.request)" :size="64" />
        <h2 class="contact-panel-name">{{ personName(activeEntry.request) }}</h2>
        <el-tag size="small" :type="statusTagType(activeEntry.request.status)">
          {{ statusLabel(activeEntry.dir, activeEntry.request.status) }}
        </el-tag>
      </div>

      <div class="contact-panel-rows">
        <div class="info-row">
          <span class="info-label">{{ activeEntry.dir === 'in' ? '验证消息' : '我的验证消息' }}</span>
          <span>{{ activeEntry.request.message || '无' }}</span>
        </div>
        <div class="info-row">
          <span class="info-label">来源</span>
          <span>{{ activeEntry.request.source }}</span>
        </div>
      </div>

      <!-- 来回回复记录（对方询问、我回答、对方再回应……直到对方拒绝/接受） -->
      <div v-if="activeEntry.request.thread?.length" class="request-thread">
        <div v-for="(msg, i) in activeEntry.request.thread" :key="i" class="thread-msg" :class="msg.from">
          <span class="thread-bubble">{{ msg.text }}</span>
        </div>
      </div>

      <!-- 对方已回复（等待我回应）：可继续回复对方 -->
      <div v-if="activeEntry.dir === 'out' && activeEntry.request.status === 'replied'" class="request-reply-editor">
        <el-input v-model="replyText" placeholder="回复对方…" @keydown.enter.prevent="sendReply" />
        <el-button type="primary" @click="sendReply">回复</el-button>
      </div>

      <!-- 收到的待处理申请：接受前询问向其开放的权限（§6，仅个人空间；组织空间走邀请码流程无此概念） -->
      <template v-if="activeEntry.dir === 'in' && activeEntry.request.status === 'pending'">
        <div v-if="spaceType === 'personal'" class="request-permission">
          <span class="request-permission-title">向其开放的权限</span>
          <el-radio-group v-model="permission" class="permission-group">
            <el-radio value="open">
              开放
              <p class="hint">朋友可以查看你允许公开的数据（含未来插件的可选公开数据）。</p>
            </el-radio>
            <el-radio value="chatOnly">
              仅聊天
              <p class="hint">朋友只能看到你的头像和昵称。</p>
            </el-radio>
          </el-radio-group>
          <!-- TODO(mock): 按插件细分的子开关（§6.2）待插件数据共享落地后实现 -->
        </div>
        <div class="request-detail-actions">
          <el-button type="primary" @click="accept">接受</el-button>
          <el-button @click="ignore">忽略</el-button>
        </div>
      </template>

      <!-- 我发出的：连接失败（对方可能离线），可重试 -->
      <div v-else-if="activeEntry.dir === 'out' && activeEntry.request.status === 'failed'" class="request-failed">
        <p class="hint">连接失败，对方可能离线。请求已保留，可在对方上线后重试。</p>
        <div class="request-detail-actions">
          <el-button type="primary" @click="emit('retry', activeEntry.request.id)">重试</el-button>
        </div>
      </div>

      <!-- 我发出的组织邀请：等待对方凭码加入期间可再复制邀请码 -->
      <div
        v-else-if="activeEntry.dir === 'out' && activeEntry.request.inviteCode && activeEntry.request.status === 'pending'"
        class="request-detail-actions"
      >
        <el-button type="primary" @click="copyInvite(activeEntry.request.inviteCode)">复制邀请码</el-button>
      </div>

      <!-- 我发出的：等待确认中也可重新发送（对方接受的回执可能丢失；
           对方未处理则重新收到申请，已通过则回发确认、双方落成朋友） -->
      <div
        v-else-if="activeEntry.dir === 'out' && activeEntry.request.status === 'pending'"
        class="request-detail-actions"
      >
        <el-button type="primary" @click="emit('retry', activeEntry.request.id)">重新发送</el-button>
      </div>
    </div>
    <el-empty v-else class="contacts-detail-empty" :image-size="110" description="选择左侧申请查看详情" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { ArrowDown, BottomLeft, TopRight } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import ContactPanel from './ContactPanel.vue';
import { personAvatarSource, personDisplayName } from '../../stores/avatar-sources';
import { friendContactItem } from './contact-item';
import { useContactActions } from './use-contact-actions';
import {
  contactsOf,
  friendOf,
  markRequestRead,
  profileOf,
  type ContactProfile,
  type FriendPermission,
  type FriendRequest
} from '../../mock/contacts';

type Direction = 'in' | 'out';
type Filter = 'all' | Direction;

export default defineComponent({
  name: 'NewFriendsPanel',
  components: { UserAvatar, ContactPanel, ArrowDown, BottomLeft, TopRight },
  props: {
    /** 收到的申请 */
    requests: { type: Array as PropType<FriendRequest[]>, required: true },
    /** 我发出的申请/邀请（含对方反应） */
    outgoing: { type: Array as PropType<FriendRequest[]>, required: true },
    spaceType: { type: String as PropType<'personal' | 'org'>, required: true },
    /** 所属空间 key（查看详情时清除未读） */
    spaceKey: { type: String, required: true }
  },
  emits: ['resolve', 'retry', 'reply'],
  setup(props, { emit }) {
    const activeKey = ref('');
    const filter = ref<Filter>('all');
    const replyText = ref('');
    /** 接受时向其开放的权限（§6），每次切换申请重置为默认「开放」 */
    const permission = ref<FriendPermission>('open');

    const rowKey = (dir: Direction, id: string) => `${dir}:${id}`;

    /** 收发混排（按最近状态变化时间倒序，新变化冒泡到顶部）+ 筛选 */
    const entries = computed(() => {
      const merged = [
        ...props.requests.map((request) => ({ dir: 'in' as Direction, request })),
        ...props.outgoing.map((request) => ({ dir: 'out' as Direction, request }))
      ].sort((a, b) => b.request.updatedAt - a.request.updatedAt);
      return filter.value === 'all' ? merged : merged.filter((entry) => entry.dir === filter.value);
    });

    const filterLabel = computed(() => {
      if (filter.value === 'in') return '收到的申请';
      if (filter.value === 'out') return props.spaceType === 'org' ? '我发出的邀请' : '我发出的';
      return '全部';
    });

    const onFilterCommand = (command: string) => {
      filter.value = command as Filter;
    };

    const activeEntry = computed(
      () => entries.value.find((entry) => rowKey(entry.dir, entry.request.id) === activeKey.value) ?? null
    );

    /** 查看详情即清除该条未读 */
    const markRead = (entry: { request: FriendRequest } | null) => {
      if (entry?.request.unread) {
        markRequestRead(props.spaceKey, entry.request.id);
      }
    };

    // 默认选中第一条；列表变化（处理完一条、切换筛选等）时保持选中有效
    watch(
      () => entries.value.map((entry) => rowKey(entry.dir, entry.request.id)).join(','),
      () => {
        if (!entries.value.some((entry) => rowKey(entry.dir, entry.request.id) === activeKey.value)) {
          const first = entries.value[0];
          activeKey.value = first ? rowKey(first.dir, first.request.id) : '';
          markRead(first ?? null);
        }
      },
      { immediate: true }
    );

    const selectRequest = (dir: Direction, id: string) => {
      activeKey.value = rowKey(dir, id);
      permission.value = 'open';
      replyText.value = '';
      markRead(entries.value.find((entry) => rowKey(entry.dir, entry.request.id) === activeKey.value) ?? null);
    };

    // ---- 已接受（个人空间）：详情栏直接内嵌联系人资料卡（可编辑备注等），
    // 状态迁移为 accepted 后自动切换，无需再跳转联系人页 ----

    /** 列表行/详情头头像：统一入口（朋友记录优先、申请快照兜底，申请人还不是朋友也能显示快照） */
    const personImage = (request: FriendRequest): string =>
      personAvatarSource(props.spaceKey, request.rootId, { image: request.avatar }).image;

    /** 列表行/详情头名称：统一展示名入口（备注>昵称>申请快照昵称） */
    const personName = (request: FriendRequest): string =>
      personDisplayName(props.spaceKey, request.rootId, request.nickname);

    /** 当前选中条目对应的朋友记录（仅已接受 + 个人空间 + 朋友关系仍在时存在） */
    const acceptedFriend = computed(() => {
      const entry = activeEntry.value;
      if (!entry || entry.request.status !== 'accepted' || props.spaceType !== 'personal') {
        return null;
      }
      return friendOf(props.spaceKey, entry.request.rootId) ?? null;
    });

    // 与通讯录列表同一映射（components/contacts/contact-item.ts）：自己取本地真实资料，其余用内核朋友记录
    const acceptedContact = computed(() => (acceptedFriend.value ? friendContactItem(acceptedFriend.value) : null));

    const acceptedProfile = computed<ContactProfile | null>(() =>
      acceptedFriend.value ? profileOf(props.spaceKey, acceptedFriend.value.rootId) : null
    );

    const tags = computed(() => contactsOf(props.spaceKey).tags);

    // 资料卡动作（备注/拉黑/删除/发消息/新建标签）与分组下拉收口在 use-contact-actions；
    // 本面板无选中态，删除后无需收尾（不传 onDeleted）
    const contactActions = useContactActions({
      spaceKey: computed(() => props.spaceKey),
      contact: acceptedContact
    });
    const groupOptions = contactActions.personalGroupOptions;
    const onSaveProfile = contactActions.saveProfile;
    const onSetBlocked = contactActions.setBlocked;
    const onDeleteFriend = contactActions.deleteFriend;
    const onSendMessage = contactActions.sendMessage;
    const onCreateTag = contactActions.createTag;

    const accept = () => {
      if (activeEntry.value?.dir === 'in') {
        emit('resolve', activeEntry.value.request.id, true, permission.value);
      }
    };

    const ignore = () => {
      if (activeEntry.value?.dir === 'in') {
        emit('resolve', activeEntry.value.request.id, false, permission.value);
      }
    };

    /** 回复对方的询问（双方可继续互复，直到对方拒绝/接受） */
    const sendReply = () => {
      if (activeEntry.value?.dir === 'out' && replyText.value.trim()) {
        emit('reply', activeEntry.value.request.id, replyText.value.trim());
        replyText.value = '';
      }
    };

    const copyInvite = async (inviteCode: string) => {
      try {
        await navigator.clipboard.writeText(inviteCode);
        ElMessage.success('邀请码已复制');
      } catch {
        ElMessage.warning('复制失败，请手动选择文本复制');
      }
    };

    const statusLabel = (dir: Direction, status: FriendRequest['status']): string => {
      if (dir === 'out') {
        if (status === 'pending') return '等待确认';
        if (status === 'accepted') return '已接受';
        if (status === 'replied') return '对方已回复';
        if (status === 'failed') return '连接失败';
        if (status === 'declined') return '已拒绝';
        return '对方已拒绝';
      }
      if (status === 'pending') return '等待处理';
      if (status === 'accepted') return '已接受';
      if (status === 'replied') return '已回复';
      return '已忽略';
    };

    const statusTagType = (status: FriendRequest['status']): 'success' | 'info' | 'warning' | 'danger' => {
      if (status === 'accepted') return 'success';
      if (status === 'failed') return 'danger';
      if (status === 'pending' || status === 'replied') return 'warning';
      return 'info';
    };

    /** 列表行副标题：发出的申请优先展示对方反应 */
    const rowSubtitle = (dir: Direction, request: FriendRequest): string => {
      if (dir === 'out') {
        const lastPeer = request.thread?.filter((msg) => msg.from === 'peer').pop();
        if (request.status === 'replied' && lastPeer) return lastPeer.text;
        if (request.status === 'failed') return '连接失败，对方可能离线';
        if (request.status === 'pending') {
          const lastMe = request.thread?.filter((msg) => msg.from === 'me').pop();
          if (lastMe) return `我：${lastMe.text}`;
          return request.inviteCode ? '等待对方凭邀请码加入' : '等待对方确认';
        }
        return request.message || (request.inviteCode ? '组织邀请' : '请求添加对方为朋友');
      }
      return request.message || '请求添加你为朋友';
    };

    return {
      activeKey,
      activeEntry,
      filter,
      filterLabel,
      onFilterCommand,
      permission,
      replyText,
      entries,
      rowKey,
      selectRequest,
      accept,
      ignore,
      sendReply,
      copyInvite,
      statusLabel,
      statusTagType,
      rowSubtitle,
      personImage,
      personName,
      acceptedContact,
      acceptedProfile,
      tags,
      groupOptions,
      onSaveProfile,
      onSetBlocked,
      onDeleteFriend,
      onSendMessage,
      onCreateTag,
      emit
    };
  }
});
</script>

<style scoped>
/* 标题栏：标题 + 右侧筛选下拉；滚动时吸顶 */
.request-list-header {
  display: flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: space-between;
  padding-right: 12px;
  position: sticky;
  top: 0;
  z-index: 2;
  background: var(--spark-bg-card);
}

.request-filter {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  font-size: 12px;
  color: var(--spark-text-2);
  cursor: pointer;
  outline: none;
}

.request-filter:hover {
  color: var(--spark-primary);
}

/* 方向标识：收到=绿（箭头指向左下/向我），发出=蓝（箭头指向右上/向外） */
.dir-icon {
  position: relative;
  flex-shrink: 0;
}

.dir-icon.in {
  color: var(--spark-success);
}

.dir-icon.out {
  color: var(--spark-primary);
}

/* 未读新变化：头像右上角红点（比方向图标角点更显眼，与会话未读红点同风格） */
.request-avatar {
  position: relative;
  flex-shrink: 0;
}

.request-avatar.unread::after {
  content: '';
  position: absolute;
  top: -2px;
  right: -2px;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 2px solid var(--spark-bg-card);
  background: var(--spark-danger);
}

/* 来回回复记录：对方靠左灰底，我靠右蓝底 */
.request-thread {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 16px;
}

.thread-msg {
  display: flex;
}

.thread-msg.me {
  justify-content: flex-end;
}

.thread-bubble {
  max-width: 80%;
  padding: 8px 12px;
  border-radius: var(--radius-sm, 6px);
  font-size: 13px;
  line-height: 1.5;
  background: var(--spark-bg-hover);
  color: var(--spark-text-1);
}

.thread-msg.me .thread-bubble {
  background: var(--spark-primary-light);
  color: var(--spark-primary);
}

/* 回复对方输入行 */
.request-reply-editor {
  display: flex;
  gap: 8px;
  margin-top: 12px;
}
</style>
