<!-- 职责：联系人详情面板（通讯录第四栏 / 标签成员抽屉），ui-contacts §5/§6/§7：
     顶部=对方资料与不可改字段（头像/昵称+性别图标/签名/角色/加入时间），没有内容的字段直接隐藏，
     RootID 属隐私不展示；中部=我可编辑的本地资料（备注名/电话/标签/分组/备忘/照片/权限，
     §5.4 均仅自己可见），点击字段值即进入编辑态，右侧出现「保存/取消」，逐字段单独保存；
     底部=发送消息 / 黑名单 / 删除 -->
<template>
  <div class="contact-panel">
    <!-- 顶部：对方资料（只读，空字段隐藏；RootID 属隐私不展示） -->
    <div class="contact-panel-hero">
      <UserAvatar :root-id="contact.avatarSeed ?? contact.rootId" :nickname="contact.displayName" :avatar="contact.avatarImage ?? ''" :size="72" />
      <h2 class="contact-panel-name">
        {{ contact.displayName }}
        <el-icon v-if="contact.gender === 'male'" class="gender-icon gender-male" :size="16"><Male /></el-icon>
        <el-icon v-else-if="contact.gender === 'female'" class="gender-icon gender-female" :size="16"><Female /></el-icon>
      </h2>
      <!-- 设置了备注名时，对方昵称单独显示一行 -->
      <p v-if="profile.remark && contact.nickname" class="contact-panel-meta">昵称：{{ contact.nickname }}</p>
      <p v-if="contact.signature" class="contact-panel-signature">{{ contact.signature }}</p>
      <p v-if="spaceType === 'org'" class="contact-panel-identity">
        <el-tag :type="contact.role === 'admin' ? 'primary' : 'info'" size="small" effect="plain">
          {{ contact.role === 'admin' ? '管理员' : '成员' }}
        </el-tag>
      </p>
      <p v-if="spaceType === 'org' && contact.joinedAt" class="contact-panel-meta">
        {{ formatDate(contact.joinedAt) }} 加入组织
      </p>
      <el-tag v-if="contact.blocked" type="danger" effect="plain" size="small">已加入黑名单</el-tag>
    </div>

    <!-- 中部：可编辑字段（点击值进入编辑态，右侧保存/取消，逐字段单独保存） -->
    <div class="contact-panel-rows">
      <!-- 备注名 -->
      <div class="info-row edit-row">
        <span class="info-label">备注名</span>
        <template v-if="editing === 'remark'">
          <el-input
            v-model="draftText"
            class="edit-input"
            size="small"
            placeholder="本地显示优先于对方昵称"
            @keyup.enter="saveEdit"
          />
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span v-else class="edit-value" :class="{ 'edit-value-empty': !profile.remark }" @click="startEdit('remark')">
          {{ profile.remark || '未设置' }}
        </span>
      </div>

      <!-- 电话（每行一个） -->
      <div class="info-row edit-row">
        <span class="info-label">电话</span>
        <template v-if="editing === 'phones'">
          <el-input
            v-model="draftText"
            class="edit-input"
            type="textarea"
            :rows="2"
            placeholder="每行一个号码，仅自己可见"
          />
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span
          v-else
          class="edit-value"
          :class="{ 'edit-value-empty': !profile.phones.length }"
          @click="startEdit('phones')"
        >
          {{ profile.phones.length ? profile.phones.join('、') : '未设置' }}
        </span>
      </div>

      <!-- 标签（可多选，可直接输入创建新标签） -->
      <div class="info-row edit-row">
        <span class="info-label">标签</span>
        <template v-if="editing === 'tags'">
          <el-select
            v-model="draftTagValues"
            class="edit-input"
            size="small"
            multiple
            filterable
            allow-create
            default-first-option
            placeholder="选择标签，或直接输入新标签名"
          >
            <el-option v-for="tag in allTags" :key="tag.id" :label="tag.name" :value="tag.id" />
          </el-select>
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span
          v-else
          class="edit-value"
          :class="{ 'edit-value-empty': !tagNames.length }"
          @click="startEdit('tags')"
        >
          {{ tagNames.length ? tagNames.join('、') : '未设置' }}
        </span>
      </div>

      <!-- 分组：个人空间所有人可改；组织空间仅管理员可改（非管理员只读） -->
      <div class="info-row edit-row">
        <span class="info-label">分组</span>
        <template v-if="editing === 'group'">
          <el-select v-model="draftGroupId" class="edit-input" size="small">
            <el-option v-for="option in groupOptions" :key="option.id" :label="option.label" :value="option.id" />
          </el-select>
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span
          v-else
          class="edit-value"
          :class="{ 'edit-value-empty': !profile.groupId, 'edit-value-readonly': !canEditGroup }"
          @click="canEditGroup && startEdit('group')"
        >
          {{ groupName }}
        </span>
      </div>

      <!-- 备忘 -->
      <div class="info-row edit-row">
        <span class="info-label">备忘</span>
        <template v-if="editing === 'memo'">
          <el-input v-model="draftText" class="edit-input" type="textarea" :rows="2" placeholder="仅自己可见" />
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span v-else class="edit-value" :class="{ 'edit-value-empty': !profile.memo }" @click="startEdit('memo')">
          {{ profile.memo || '未设置' }}
        </span>
      </div>

      <!-- 照片（色块占位 mock，点击移除 / + 添加，即时生效无需保存） -->
      <div class="info-row info-row-photos">
        <span class="info-label">照片</span>
        <div class="photo-grid">
          <button
            v-for="(photo, index) in profile.photos"
            :key="photo"
            type="button"
            class="photo-thumb"
            :style="{ background: hashGradient(photo) }"
            title="点击移除（mock）"
            @click="removePhoto(index)"
          />
          <button type="button" class="photo-add" title="添加照片（mock）" @click="addPhoto">+</button>
        </div>
      </div>

      <!-- 权限（仅个人空间，§6：开放 / 仅聊天） -->
      <div v-if="spaceType === 'personal'" class="info-row edit-row">
        <span class="info-label">权限</span>
        <template v-if="editing === 'permission'">
          <el-select v-model="draftPermission" class="edit-input" size="small">
            <el-option label="开放（朋友可查看你公开的数据）" value="open" />
            <el-option label="仅聊天（对方只能看到你的头像和昵称）" value="chatOnly" />
          </el-select>
          <span class="edit-actions">
            <el-button size="small" type="primary" @click="saveEdit">保存</el-button>
            <el-button size="small" @click="cancelEdit">取消</el-button>
          </span>
        </template>
        <span v-else class="edit-value" @click="startEdit('permission')">
          {{ profile.permission === 'open' ? '开放' : '仅聊天' }}
        </span>
      </div>
    </div>

    <!-- 底部：操作 -->
    <div class="contact-panel-actions">
      <!-- 个人空间允许给自己发消息（同步到所有个人节点）；组织空间对自己仍禁用 -->
      <el-button
        type="primary"
        :disabled="contact.blocked || (contact.isSelf && spaceType !== 'personal')"
        @click="emit('send-message')"
      >
        发送消息
      </el-button>
      <p v-if="contact.blocked" class="hint">已拉黑：对方无法向你发送消息，发送消息不可用（§7.1）。</p>
      <el-button v-if="spaceType === 'org' && !contact.isSelf" @click="emit('add-as-friend')">
        添加为个人联系人
      </el-button>
    </div>

    <div class="contact-panel-ops">
      <button type="button" class="op-row op-danger" @click="toggleBlocked">
        {{ contact.blocked ? '移出黑名单' : '加入黑名单' }}
      </button>
      <button
        v-if="(spaceType === 'personal' || isAdmin) && !contact.isSelf"
        type="button"
        class="op-row op-danger"
        @click="emit('delete')"
      >
        {{ spaceType === 'personal' ? '删除朋友' : '删除成员' }}
      </button>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { Female, Male } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { hashGradient } from '../../utils/palette';
import type { ContactProfile, ContactTag } from '../../mock/contacts';
import type { ContactItem, GroupOption } from './types';

/** 可内联编辑的字段（照片为即时增删，不在此列） */
type EditableField = 'remark' | 'phones' | 'tags' | 'group' | 'memo' | 'permission';

export default defineComponent({
  name: 'ContactPanel',
  components: { UserAvatar, Male, Female },
  props: {
    contact: { type: Object as PropType<ContactItem>, required: true },
    spaceType: { type: String as PropType<'personal' | 'org'>, required: true },
    /** 组织空间：当前用户是否为管理员（决定「删除成员」与「分组」编辑是否可见） */
    isAdmin: { type: Boolean, default: false },
    profile: { type: Object as PropType<ContactProfile>, required: true },
    allTags: { type: Array as PropType<ContactTag[]>, required: true },
    /** 「分组」下拉选项（'' = 未分组；组织空间为树扁平化带缩进） */
    groupOptions: { type: Array as PropType<GroupOption[]>, default: () => [{ id: '', label: '未分组' }] },
    /** 新建标签（写 mock store 由父级完成，返回新标签便于立即选中） */
    onCreateTag: { type: Function as PropType<(name: string) => ContactTag>, required: true }
  },
  emits: ['save-profile', 'set-blocked', 'delete', 'send-message', 'add-as-friend'],
  setup(props, { emit }) {
    // ---- 逐字段内联编辑：同一时刻只有一个字段处于编辑态 ----
    const editing = ref<EditableField | ''>('');
    const draftText = ref('');
    const draftTagValues = ref<string[]>([]);
    const draftGroupId = ref('');
    const draftPermission = ref<'open' | 'chatOnly'>('open');

    const tagNames = computed(() => {
      const byId = new Map(props.allTags.map((tag) => [tag.id, tag.name]));
      return props.profile.tagIds.map((id) => byId.get(id)).filter((name): name is string => Boolean(name));
    });

    /** 分组编辑权：个人空间所有人；组织空间仅管理员 */
    const canEditGroup = computed(() => props.spaceType === 'personal' || props.isAdmin);

    /** 只读态展示的分组名（缩进字符去掉） */
    const groupName = computed(
      () => props.groupOptions.find((option) => option.id === props.profile.groupId)?.label.trim() || '未分组'
    );

    const formatDate = (timestamp: number) =>
      new Intl.DateTimeFormat('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).format(new Date(timestamp));

    const startEdit = (field: EditableField) => {
      editing.value = field;
      if (field === 'remark') {
        draftText.value = props.profile.remark;
      } else if (field === 'phones') {
        draftText.value = props.profile.phones.join('\n');
      } else if (field === 'memo') {
        draftText.value = props.profile.memo;
      } else if (field === 'tags') {
        draftTagValues.value = [...props.profile.tagIds];
      } else if (field === 'group') {
        draftGroupId.value = props.profile.groupId;
      } else if (field === 'permission') {
        draftPermission.value = props.profile.permission;
      }
    };

    const cancelEdit = () => {
      editing.value = '';
    };

    /** 标签值归一：已选 id 原样保留；同名标签复用；新名称经 onCreateTag 落库 */
    const resolveTagIds = (values: string[]): string[] =>
      values.map((value) => {
        const byId = props.allTags.find((tag) => tag.id === value);
        if (byId) {
          return byId.id;
        }
        const byName = props.allTags.find((tag) => tag.name === value);
        return byName ? byName.id : props.onCreateTag(value).id;
      });

    const saveEdit = () => {
      const field = editing.value;
      if (!field) {
        return;
      }
      if (field === 'remark') {
        emit('save-profile', { remark: draftText.value.trim() });
      } else if (field === 'phones') {
        emit('save-profile', { phones: draftText.value.split('\n').map((item) => item.trim()).filter(Boolean) });
      } else if (field === 'memo') {
        emit('save-profile', { memo: draftText.value.trim() });
      } else if (field === 'tags') {
        emit('save-profile', { tagIds: resolveTagIds(draftTagValues.value) });
      } else if (field === 'group') {
        emit('save-profile', { groupId: draftGroupId.value });
      } else if (field === 'permission') {
        emit('save-profile', { permission: draftPermission.value });
      }
      editing.value = '';
      ElMessage.success('已保存');
    };

    const toggleBlocked = async () => {
      if (!props.contact.blocked) {
        try {
          await ElMessageBox.confirm(
            '加入黑名单后，对方无法向你发送消息，也无法查看你除头像和昵称外的数据（§7.1）。',
            '加入黑名单',
            { type: 'warning', confirmButtonText: '加入黑名单', cancelButtonText: '取消' }
          );
        } catch {
          return;
        }
      }
      emit('set-blocked', !props.contact.blocked);
    };

    // TODO(mock): 照片为占位色块，添加/移除只改 mock store，未接入真实图片
    const addPhoto = () => {
      emit('save-profile', { photos: [...props.profile.photos, `photo-${Date.now()}`] });
    };

    const removePhoto = (index: number) => {
      const photos = [...props.profile.photos];
      photos.splice(index, 1);
      emit('save-profile', { photos });
    };

    return {
      editing,
      draftText,
      draftTagValues,
      draftGroupId,
      draftPermission,
      tagNames,
      canEditGroup,
      groupName,
      formatDate,
      hashGradient,
      startEdit,
      cancelEdit,
      saveEdit,
      toggleBlocked,
      addPhoto,
      removePhoto,
      emit
    };
  }
});
</script>
