<!-- 朋友权限/成员权限模块（MinePage 第三、四栏）：
     个人空间「朋友权限」= 仅聊天 + 通讯黑名单两项；组织空间「成员权限」= 仅通讯黑名单。
     第三栏为名单项，第四栏管理当前名单（选择联系人添加 / 逐人移出）。
     数据与通讯录共用同一份 mock store（permission/blocked 写在联系人本地资料上），改动双向可见 -->
<template>
  <!-- 第三栏：权限名单项 -->
  <div class="mine-list">
    <h2 class="mine-list-title">{{ mode === 'personal' ? '朋友权限' : '成员权限' }}</h2>
    <div class="mine-list-items">
      <button
        v-if="mode === 'personal'"
        type="button"
        class="mine-list-item"
        :class="{ active: view === 'chatOnly' }"
        @click="view = 'chatOnly'"
      >
        <!-- 移动端菜单图标补色（微信式每项一色，同 MinePage 一级菜单色板；桌面端不生效） -->
        <el-icon class="mine-list-item-icon" :size="17" :style="{ color: '#ff7d00' }"><ChatLineRound /></el-icon>
        <span class="mine-list-item-text">
          <b>仅聊天</b>
          <span>{{ chatOnlyPeople.length }} 人 · 对方只能看到你的头像和昵称</span>
        </span>
      </button>
      <button
        type="button"
        class="mine-list-item"
        :class="{ active: view === 'blocked' }"
        @click="view = 'blocked'"
      >
        <el-icon class="mine-list-item-icon" :size="17" :style="{ color: '#64748b' }"><Remove /></el-icon>
        <span class="mine-list-item-text">
          <b>通讯黑名单</b>
          <span>{{ blockedPeople.length }} 人 · 对方无法向你发消息</span>
        </span>
      </button>
    </div>
  </div>

  <!-- 第四栏：当前名单管理（column 模式=第四栏；drawer 模式=抽屉，设置页复用） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="view !== null"
    :title="viewTitle"
    @close="view = null"
  >
    <div class="perm-body">
      <h2 class="perm-title">{{ view === 'chatOnly' ? '仅聊天' : '通讯黑名单' }}</h2>
      <p class="hint">
        {{ view === 'chatOnly'
          ? '仅聊天的朋友只能查看你的头像和昵称，其余资料不会发送给对方。'
          : '黑名单中的联系人无法向你发送消息，也无法查看你除头像和昵称外的数据。' }}
      </p>
      <div class="perm-add-row">
        <el-select v-model="picked" class="perm-add-select" filterable placeholder="选择联系人加入">
          <el-option v-for="p in addablePeople" :key="p.rootId" :label="p.name" :value="p.rootId" />
        </el-select>
        <el-button type="primary" :disabled="!picked" @click="add">添加</el-button>
      </div>
      <el-empty v-if="!currentPeople.length" :image-size="90" description="暂无联系人" />
      <div v-for="p in currentPeople" :key="p.rootId" class="perm-row">
        <UserAvatar :root-id="p.avatarSeed ?? p.rootId" :nickname="p.name" :avatar="p.avatarImage ?? ''" :size="36" />
        <span class="perm-name">{{ p.name }}</span>
        <el-button text size="small" type="danger" @click="remove(p)">
          {{ view === 'chatOnly' ? '移出' : '移出黑名单' }}
        </el-button>
      </div>
    </div>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { ChatLineRound, Remove } from '@element-plus/icons-vue';
import { currentSpace, currentSpaceOrgId } from '../../stores/current-space';
import { findOrg, refreshOrganizations } from '../../stores/org-membership';
import {
  orgMemberAvatarSource,
  orgMemberDisplayName,
  personAvatarSource,
  personDisplayName
} from '../../stores/avatar-sources';
import { contactsOf, profileOf, setBlocked, spaceKeyOf, updateProfile } from '../../mock/contacts';
import UserAvatar from '../UserAvatar.vue';
import MineDetailContainer from './MineDetailContainer.vue';

type Person = {
  rootId: string;
  name: string;
  blocked: boolean;
  permission: string;
  /** 头像配色种子：组织成员=rootId@orgId；缺省=rootId */
  avatarSeed?: string;
  /** 已上传的头像图片（dataURL）；空/缺省=自动配色头像 */
  avatarImage?: string;
};

const shortRootId = (rootId: string) => `${rootId.slice(0, 10)}...`;

export default defineComponent({
  name: 'PermissionModule',
  components: { ChatLineRound, Remove, UserAvatar, MineDetailContainer },
  props: {
    mode: { type: String as PropType<'personal' | 'org'>, required: true },
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持现有默认选中
    const view = ref<'chatOnly' | 'blocked' | null>(
      props.detailMode === 'drawer' ? null : props.mode === 'personal' ? 'chatOnly' : 'blocked'
    );
    const picked = ref('');
    /** 组织空间成员名单（真实 listMine，经 org-membership 共享缓存；个人空间直接读 mock 朋友列表） */
    const orgMembers = ref<Array<{ rootId: string }>>([]);

    const spaceKey = computed(() => spaceKeyOf(currentSpace.value));

    onMounted(async () => {
      if (props.mode !== 'org') {
        return;
      }
      try {
        await refreshOrganizations();
        orgMembers.value = findOrg(currentSpaceOrgId.value)?.members ?? [];
      } catch {
        orgMembers.value = [];
      }
    });

    /** 统一的人员视图：个人=mock 朋友；组织=真实成员 + 本地附加资料；名称/头像走统一入口 */
    const people = computed<Person[]>(() => {
      if (props.mode === 'personal') {
        return contactsOf('personal').friends.map((friend) => ({
          rootId: friend.rootId,
          name: personDisplayName('personal', friend.rootId),
          blocked: friend.blocked,
          permission: friend.permission,
          avatarImage: personAvatarSource('personal', friend.rootId).image
        }));
      }
      return orgMembers.value.map((member) => {
        const profile = profileOf(spaceKey.value, member.rootId);
        return {
          rootId: member.rootId,
          name: orgMemberDisplayName(currentSpaceOrgId.value, member.rootId, shortRootId(member.rootId)),
          blocked: profile.blocked,
          permission: profile.permission,
          avatarSeed: orgMemberAvatarSource(currentSpaceOrgId.value, member.rootId).seed
        };
      });
    });

    const chatOnlyPeople = computed(() => people.value.filter((p) => p.permission === 'chatOnly' && !p.blocked));
    const blockedPeople = computed(() => people.value.filter((p) => p.blocked));
    const currentPeople = computed(() => (view.value === 'chatOnly' ? chatOnlyPeople.value : blockedPeople.value));
    /** 抽屉标题：当前选中名单名 */
    const viewTitle = computed(() => (view.value === null ? '' : view.value === 'chatOnly' ? '仅聊天' : '通讯黑名单'));
    const addablePeople = computed(() =>
      view.value === 'chatOnly'
        ? people.value.filter((p) => p.permission !== 'chatOnly' && !p.blocked)
        : people.value.filter((p) => !p.blocked)
    );

    const add = () => {
      if (!picked.value) {
        return;
      }
      if (view.value === 'chatOnly') {
        updateProfile(spaceKey.value, picked.value, { permission: 'chatOnly' });
        ElMessage.success('已设为仅聊天');
      } else {
        setBlocked(spaceKey.value, picked.value, true);
        ElMessage.success('已加入黑名单');
      }
      picked.value = '';
    };

    const remove = (person: Person) => {
      if (view.value === 'chatOnly') {
        updateProfile(spaceKey.value, person.rootId, { permission: 'open' });
        ElMessage.success('已恢复为开放');
      } else {
        setBlocked(spaceKey.value, person.rootId, false);
        ElMessage.success('已移出黑名单');
      }
    };

    return {
      view,
      viewTitle,
      picked,
      chatOnlyPeople,
      blockedPeople,
      currentPeople,
      addablePeople,
      add,
      remove
    };
  }
});
</script>

<style scoped>
.perm-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-width: 640px;
  margin: 0 auto;
}

.perm-title {
  margin: 0;
  /* 与 .mine-detail 卡片标题统一（16px/600） */
  font-size: 16px;
  font-weight: 600;
  color: var(--spark-text-1);
}

.perm-add-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.perm-add-select {
  flex: 1;
  min-width: 0;
}

/* 名单行：58px 贴边行，hover 灰底 */
.perm-row {
  display: flex;
  align-items: center;
  gap: 12px;
  height: 58px;
  padding: 0 16px;
}

.perm-row:hover {
  background: var(--spark-bg-hover);
}

.perm-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--spark-text-1);
  font-size: 14px;
}
</style>
