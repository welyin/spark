<template>
  <!-- 第三栏：标签列表 -->
  <div class="contacts-request-list">
    <div class="contacts-request-title">
      <span>标签</span>
      <button class="request-head-btn" title="新建标签" @click="createTagRow">
        <el-icon :size="16"><Plus /></el-icon>
      </button>
    </div>
    <el-empty v-if="!tags.length" :image-size="90" description="暂无标签" />
    <button
      v-for="tag in tags"
      :key="tag.id"
      class="request-item"
      :class="{ active: activeId === tag.id }"
      @click="activeId = tag.id"
    >
      <span class="row-icon">
        <el-icon :size="17"><CollectionTag /></el-icon>
      </span>
      <span class="request-item-main">
        <b>{{ tag.name }}</b>
        <span>{{ memberCountOf(tag) }} 人</span>
      </span>
    </button>
  </div>

  <!-- 第四栏：标签成员管理 -->
  <div class="contacts-detail">
    <div v-if="activeTag" class="request-profile">
      <div class="tag-detail-header">
        <h2 class="tag-detail-name">{{ activeTag.name }}</h2>
        <div class="tag-detail-actions">
          <el-button text size="small" @click="renameTagRow(activeTag)">重命名</el-button>
          <el-button text size="small" type="danger" @click="deleteTagRow(activeTag)">删除标签</el-button>
        </div>
      </div>
      <div class="tag-add-row">
        <el-select
          v-model="picked"
          class="tag-add-select"
          multiple
          collapse-tags
          filterable
          placeholder="选择联系人加入该标签"
        >
          <el-option v-for="c in addableContacts" :key="c.rootId" :label="c.displayName" :value="c.rootId" />
        </el-select>
        <el-button type="primary" :disabled="!picked.length" @click="addMembers">添加</el-button>
      </div>
      <el-empty v-if="!members.length" :image-size="90" description="该标签下暂无成员" />
      <!-- 成员行：点击打开抽屉展示联系人详情（同通讯录资料卡）；「移除」阻止冒泡 -->
      <div
        v-for="m in members"
        :key="m.rootId"
        class="request-item tag-member-row tag-member-clickable"
        @click="$emit('view-member', m.rootId)"
      >
        <UserAvatar :root-id="m.rootId" :nickname="m.displayName" :size="36" />
        <span class="request-item-main"><b>{{ m.displayName }}</b></span>
        <el-button text size="small" type="danger" @click.stop="removeMember(m.rootId)">移除</el-button>
      </div>
    </div>
    <el-empty v-else class="contacts-detail-empty" :image-size="110" description="选择左侧标签管理成员" />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import { ElMessageBox } from 'element-plus';
import { CollectionTag, Plus } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { createTag, deleteTag, profileOf, renameTag, type ContactTag } from '../../mock/contacts';
import type { ContactItem } from './types';

export default defineComponent({
  name: 'TagManager',
  components: { CollectionTag, Plus, UserAvatar },
  props: {
    contacts: { type: Array as PropType<ContactItem[]>, required: true },
    tags: { type: Array as PropType<ContactTag[]>, required: true },
    spaceKey: { type: String, required: true }
  },
  emits: ['view-member'],
  setup(props) {
    const activeId = ref('');
    const picked = ref<string[]>([]);

    const activeTag = computed(() => props.tags.find((t) => t.id === activeId.value) || null);
    // 成员关系存于各联系人本地资料的 tagIds（设计 §8）
    const members = computed(() => {
      const tag = activeTag.value;
      if (!tag) return [];
      return props.contacts.filter((c) => profileOf(props.spaceKey, c.rootId).tagIds.includes(tag.id));
    });
    const addableContacts = computed(() => {
      const tag = activeTag.value;
      if (!tag) return [];
      return props.contacts.filter((c) => !profileOf(props.spaceKey, c.rootId).tagIds.includes(tag.id));
    });

    watch(
      () => props.tags,
      (list) => {
        if (!list.some((t) => t.id === activeId.value)) {
          activeId.value = list.length ? list[0].id : '';
        }
      },
      { immediate: true }
    );
    watch(activeId, () => {
      picked.value = [];
    });

    const memberCountOf = (tag: ContactTag) =>
      props.contacts.filter((c) => profileOf(props.spaceKey, c.rootId).tagIds.includes(tag.id)).length;

    const addMembers = () => {
      const tag = activeTag.value;
      if (!tag || !picked.value.length) return;
      for (const rootId of picked.value) {
        const profile = profileOf(props.spaceKey, rootId);
        if (!profile.tagIds.includes(tag.id)) profile.tagIds.push(tag.id);
      }
      picked.value = [];
    };

    const removeMember = (rootId: string) => {
      const tag = activeTag.value;
      if (!tag) return;
      const profile = profileOf(props.spaceKey, rootId);
      profile.tagIds = profile.tagIds.filter((id) => id !== tag.id);
    };

    const createTagRow = () => {
      ElMessageBox.prompt('请输入标签名称', '新建标签', {
        confirmButtonText: '创建',
        cancelButtonText: '取消',
        inputPlaceholder: '例如：家人、同事'
      })
        .then(({ value }) => {
          const name = (value || '').trim();
          if (!name) return;
          activeId.value = createTag(props.spaceKey, name).id;
        })
        .catch(() => {});
    };

    const renameTagRow = (tag: ContactTag) => {
      ElMessageBox.prompt('请输入新的标签名称', '重命名标签', {
        confirmButtonText: '保存',
        cancelButtonText: '取消',
        inputValue: tag.name
      })
        .then(({ value }) => {
          const name = (value || '').trim();
          if (name) renameTag(props.spaceKey, tag.id, name);
        })
        .catch(() => {});
    };

    const deleteTagRow = (tag: ContactTag) => {
      ElMessageBox.confirm(`确定删除标签「${tag.name}」吗？`, '删除标签', {
        confirmButtonText: '删除',
        cancelButtonText: '取消',
        type: 'warning'
      })
        .then(() => {
          deleteTag(props.spaceKey, tag.id);
        })
        .catch(() => {});
    };

    return {
      activeId,
      picked,
      activeTag,
      members,
      addableContacts,
      memberCountOf,
      addMembers,
      removeMember,
      createTagRow,
      renameTagRow,
      deleteTagRow
    };
  }
});
</script>
