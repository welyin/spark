<!-- 扁平联系人行列表：58px 行（.request-item），头像 + 名称（搜索词 <mark> 高亮、
     角色标签）+ 副标题。复用处：第二栏搜索结果、第三栏分组成员。
     group-by-letter 时按拼音首字母分组渲染（A..Z、'#' 兜底，§2.3，分组头 sticky） -->
<template>
  <div class="contact-list">
    <el-empty v-if="items.length === 0" :image-size="90" :description="emptyText" />
    <template v-else>
      <template v-for="row in rows" :key="row.type === 'header' ? `letter-${row.letter}` : row.item.rootId">
        <div v-if="row.type === 'header'" class="contact-letter-header">{{ row.letter }}</div>
        <button
          v-else
          class="request-item"
          :class="{ active: row.item.rootId === activeRootId }"
          type="button"
          @click="emit('select', row.item)"
        >
          <UserAvatar :root-id="row.item.avatarSeed ?? row.item.rootId" :nickname="row.item.displayName" :avatar="row.item.avatarImage ?? ''" :size="36" />
          <span class="request-item-main">
            <b>
              <template v-for="(seg, j) in segments(row.item.displayName)" :key="j">
                <mark v-if="seg.hit">{{ seg.text }}</mark>
                <template v-else>{{ seg.text }}</template>
              </template>
              <el-icon v-if="row.item.gender === 'male'" class="gender-icon gender-male" :size="14"><Male /></el-icon>
              <el-icon v-else-if="row.item.gender === 'female'" class="gender-icon gender-female" :size="14"><Female /></el-icon>
            </b>
            <span v-if="row.item.subtitle">{{ row.item.subtitle }}</span>
          </span>
          <!-- 角色标签（管理员/成员）：行尾上下居中 -->
          <el-tag v-if="row.item.role === 'admin'" class="contact-role-tag contact-role-side" size="small" effect="plain">管理员</el-tag>
          <el-tag v-else-if="row.item.role === 'member'" class="contact-role-tag contact-role-side" size="small" effect="plain" type="info">成员</el-tag>
        </button>
      </template>
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { Female, Male } from '@element-plus/icons-vue';
import UserAvatar from '../UserAvatar.vue';
import { compareLetters, firstLetter } from '../../utils/pinyin';
import type { ContactItem } from './types';

type ListRow = { type: 'header'; letter: string } | { type: 'item'; item: ContactItem };

export default defineComponent({
  name: 'ContactList',
  components: { UserAvatar, Male, Female },
  props: {
    items: { type: Array as PropType<ContactItem[]>, required: true },
    /** 当前选中联系人（行高亮） */
    activeRootId: { type: String, default: '' },
    /** 搜索词：命中片段 <mark> 高亮（§2.4） */
    keyword: { type: String, default: '' },
    /** 按拼音首字母分组渲染（§2.3，组内保持传入顺序——调用方已按 compareNames 排序） */
    groupByLetter: { type: Boolean, default: false },
    emptyText: { type: String, default: '暂无联系人' }
  },
  emits: ['select'],
  setup(props, { emit }) {
    /** 行序列：groupByLetter 时在每组前插入字母头（A..Z，'#' 恒最后），否则纯联系人行 */
    const rows = computed<ListRow[]>(() => {
      if (!props.groupByLetter) {
        return props.items.map((item) => ({ type: 'item', item }));
      }
      const byLetter = new Map<string, ContactItem[]>();
      for (const item of props.items) {
        const letter = firstLetter(item.displayName);
        const bucket = byLetter.get(letter);
        if (bucket) {
          bucket.push(item);
        } else {
          byLetter.set(letter, [item]);
        }
      }
      const letters = [...byLetter.keys()].sort(compareLetters);
      return letters.flatMap<ListRow>((letter) => [
        { type: 'header', letter },
        ...byLetter.get(letter)!.map((item) => ({ type: 'item' as const, item }))
      ]);
    });
    /** 名称按搜索词切片，命中段渲染 <mark>（大小写不敏感） */
    const segments = (name: string): Array<{ text: string; hit: boolean }> => {
      const keyword = props.keyword.trim().toLowerCase();
      if (!keyword) {
        return [{ text: name, hit: false }];
      }
      const result: Array<{ text: string; hit: boolean }> = [];
      let rest = name;
      while (rest) {
        const index = rest.toLowerCase().indexOf(keyword);
        if (index < 0) {
          result.push({ text: rest, hit: false });
          break;
        }
        if (index > 0) {
          result.push({ text: rest.slice(0, index), hit: false });
        }
        result.push({ text: rest.slice(index, index + keyword.length), hit: true });
        rest = rest.slice(index + keyword.length);
      }
      return result;
    };

    return { rows, segments, emit };
  }
});
</script>
