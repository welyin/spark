<!--
  朋友圈插件（spark-moments）· 名单编辑器（composer.md §4.3）。
  联系人/分组/标签三标签页，勾选即时反映到已选条（去重计数），「完成」返回四选一页。
  勾选在发表时经 contacts.listFriends/listGroups/listTags 统一展开为 rootId 集合（service）。
  数据源 contact:read：未授权时整体替换为授权引导页。
-->
<template>
  <section class="editor">
    <header class="top">
      <button type="button" class="back" @click="pop">‹ 返回</button>
      <span class="title">{{ title }}</span>
      <span class="placeholder"></span>
    </header>

    <template v-if="denied">
      <div class="denied">
        <p>选择可见范围需要读取通讯录，仅用于展开本条动态的投递名单。</p>
        <el-button type="primary" @click="retryAuth">去授权</el-button>
      </div>
    </template>

    <template v-else>
      <div class="tabs">
        <button
          v-for="tab in tabs"
          :key="tab.key"
          type="button"
          class="tab"
          :class="{ active: activeTab === tab.key }"
          @click="activeTab = tab.key"
        >{{ tab.label }}</button>
      </div>

      <div class="list">
        <!-- 联系人 -->
        <label v-if="activeTab === 'contacts'" v-for="f in friends" :key="f.rootId" class="row">
          <input type="checkbox" :checked="isContactChecked(f.rootId)" @change="toggleContact(f.rootId)" />
          <span class="name">{{ displayName(f) }}</span>
        </label>
        <!-- 分组 -->
        <label v-if="activeTab === 'groups'" v-for="g in groups" :key="g.id" class="row">
          <input type="checkbox" :checked="selection.groupIds.includes(g.id)" @change="toggleGroup(g.id)" />
          <span class="name">{{ g.name }}</span>
          <span class="count">{{ countInGroup(g.id) }} 人</span>
        </label>
        <!-- 标签 -->
        <label v-if="activeTab === 'tags'" v-for="t in tags" :key="t.id" class="row">
          <input type="checkbox" :checked="selection.tagIds.includes(t.id)" @change="toggleTag(t.id)" />
          <span class="name">{{ t.name }}</span>
          <span class="count">{{ countInTag(t.id) }} 人</span>
        </label>
      </div>

      <footer class="footer">
        <span class="count-label">已选 {{ selectedCount }} 人</span>
        <button type="button" class="done" @click="pop">完成</button>
      </footer>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { usePageStack } from '../composables/usePageStack';
import { useMoments } from '../composables/useMoments';
import { visibilityState } from './visibilityState';
import type { PluginFriendSummary } from '../../../packages/plugin-sdk/src';

export default defineComponent({
  name: 'RecipientEditorView',
  props: {
    title: { type: String, default: '名单编辑' }
  },
  setup() {
    const { pop } = usePageStack();
    const moments = useMoments();
    const activeTab = ref<'contacts' | 'groups' | 'tags'>('contacts');
    const denied = ref(false);
    const friends = ref<PluginFriendSummary[]>([]);
    const groups = ref<{ id: string; name: string }[]>([]);
    const tags = ref<{ id: string; name: string }[]>([]);

    const selection = computed(() => visibilityState.selection);
    const selectedCount = computed(() => {
      // 已选条反映去重后人数：联系人 + 分组/标签展开成员
      const s = visibilityState.selection;
      return s.contactRootIds.length + s.groupIds.length + s.tagIds.length;
    });

    const tabs = [
      { key: 'contacts' as const, label: '联系人' },
      { key: 'groups' as const, label: '分组' },
      { key: 'tags' as const, label: '标签' }
    ];

    onMounted(async () => {
      try {
        const c = await moments.loadContacts();
        friends.value = c.friends;
        groups.value = c.groups;
        tags.value = c.tags;
      } catch {
        denied.value = true;
      }
    });

    const displayName = (f: PluginFriendSummary) => f.nickname;
    const isContactChecked = (rootId: string) => visibilityState.selection.contactRootIds.includes(rootId);
    const toggleContact = (rootId: string) => toggleIn(visibilityState.selection.contactRootIds, rootId);
    const toggleGroup = (id: string) => toggleIn(visibilityState.selection.groupIds, id);
    const toggleTag = (id: string) => toggleIn(visibilityState.selection.tagIds, id);

    function toggleIn(list: string[], item: string) {
      const idx = list.indexOf(item);
      if (idx >= 0) list.splice(idx, 1);
      else list.push(item);
    }

    const countInGroup = (groupId: string) => friends.value.filter((f) => f.groupId === groupId).length;
    const countInTag = (tagId: string) => friends.value.filter((f) => f.tagIds.includes(tagId)).length;

    const retryAuth = () => { denied.value = false; };

    return {
      title: '名单编辑',
      pop, activeTab, denied, friends, groups, tags, selection, selectedCount, tabs,
      displayName, isContactChecked, toggleContact, toggleGroup, toggleTag,
      countInGroup, countInTag, retryAuth
    };
  }
});
</script>

<style scoped>
/* 页栈子页根定高 100%（父级 .moments-root 定高不滚动）：flex 列内 .list 自滚动 */
.editor { height: 100%; overflow: hidden; background: var(--spark-bg-page, #f5f5f5); display: flex; flex-direction: column; }
.top { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); }
.back { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 16px; cursor: pointer; }
.title { font-weight: 600; }
.placeholder { width: 40px; }
.tabs { display: flex; background: var(--spark-bg-card, #fff); border-bottom: 1px solid var(--spark-border, #f1f5f9); }
.tab { flex: 1; border: none; background: none; padding: 12px; cursor: pointer; font-size: 14px; color: var(--spark-text-2, #475569); border-bottom: 2px solid transparent; }
.tab.active { color: var(--spark-primary, #4f7cff); border-bottom-color: var(--spark-primary, #4f7cff); }
.list { flex: 1; min-height: 0; overflow-y: auto; background: var(--spark-bg-card, #fff); margin: 12px; border-radius: 8px; }
.row { display: flex; align-items: center; gap: 12px; padding: 14px 16px; border-bottom: 1px solid var(--spark-border, #f1f5f9); cursor: pointer; }
.name { flex: 1; }
.count { color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.footer { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); border-top: 1px solid var(--spark-border, #f1f5f9); }
.count-label { font-size: 13px; color: var(--spark-text-2, #475569); }
.done { border: none; border-radius: 6px; background: var(--spark-primary, #4f7cff); color: #fff; padding: 8px 24px; cursor: pointer; }
.denied { padding: 40px 24px; text-align: center; color: var(--spark-text-2, #475569); }
.denied p { margin-bottom: 16px; }
</style>
