<!-- 顶栏全局搜索（纯前端模糊匹配）：分组展示 联系人/会话/应用/组织，
     点击结果跳转到目标页面（联系人资料 / 会话 / 应用详情 / 切换组织空间）。
     数据源全部复用现有 store/接口：mock/contacts + org-membership（联系人）、
     mock/messages（会话）、pluginMarket.list + mock/apps（应用）、org-membership（组织） -->
<template>
  <div class="global-search">
    <el-input
      v-model="keyword"
      class="global-search-input"
      size="small"
      placeholder="搜索联系人、会话、应用"
      clearable
      :prefix-icon="SearchIcon"
      @focus="open = true"
      @input="open = true"
      @keydown.enter.prevent="pickFirst"
      @keydown.esc.prevent="close"
      @blur="close"
    />

    <!-- mousedown.prevent 阻止输入框失焦，保证 item 的 click 先于 blur 触发 -->
    <div v-if="open && keyword.trim()" class="global-search-dropdown" @mousedown.prevent>
      <template v-if="groups.length > 0">
        <div v-for="group in groups" :key="group.label" class="gs-group">
          <div class="gs-group-title">{{ group.label }}</div>
          <button v-for="item in group.items" :key="item.key" type="button" class="gs-item" @click="select(item)">
            <UserAvatar
              v-if="item.kind === 'contact' || item.kind === 'conversation'"
              :root-id="item.avatarSeed ?? item.rootId ?? ''"
              :nickname="item.name"
              :avatar="item.avatarImage ?? ''"
              :size="28"
            />
            <OrgAvatar v-else-if="item.kind === 'org'" :org-id="item.orgId ?? ''" :name="item.name" :size="28" />
            <span v-else class="gs-app-icon" :style="{ background: item.iconBackground }">{{ item.name.slice(0, 1) }}</span>
            <span class="gs-item-main">
              <span class="gs-item-name">{{ item.name }}</span>
              <span class="gs-item-subtitle">{{ item.subtitle }}</span>
            </span>
          </button>
        </div>
      </template>
      <div v-else class="gs-empty">无匹配结果</div>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { Search } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../api/types';
import { currentSpace, switchSpace, type CurrentSpace } from '../stores/current-space';
import { organizations, refreshOrganizations } from '../stores/org-membership';
import {
  orgMemberAvatarSource,
  personAvatarSource,
  personDisplayName
} from '../stores/avatar-sources';
import { contactsOf } from '../mock/contacts';
import { listConversations, spaceKeyOf } from '../mock/messages';
import { appConversationName } from '../stores/app-conversations';
import { listMockApps } from '../mock/apps';
import { mockMode } from '../mock/mode';
import { appIconBackground, marketItemMatches } from './apps/apps-store';
import { openChat } from './contacts/open-intents';
import UserAvatar from './UserAvatar.vue';
import OrgAvatar from './OrgAvatar.vue';

/** 每组最多展示的条数 */
const GROUP_LIMIT = 5;

type SearchItem = {
  key: string;
  kind: 'contact' | 'conversation' | 'app' | 'org';
  name: string;
  subtitle: string;
  /** 联系人/会话所属空间（跳转前先切空间） */
  space?: CurrentSpace;
  rootId?: string;
  conversationId?: string;
  pluginId?: string;
  orgId?: string;
  iconBackground?: string;
  /** 头像配色种子：组织成员=rootId@orgId；缺省=rootId */
  avatarSeed?: string;
  /** 已上传的头像图片（dataURL）；空/缺省=自动配色头像 */
  avatarImage?: string;
};

type SearchGroup = { label: string; items: SearchItem[] };

const shortRootId = (rootId: string) => `${rootId.slice(0, 10)}...`;
const matches = (keyword: string, ...fields: string[]) =>
  fields.join('\n').toLowerCase().includes(keyword);

export default defineComponent({
  name: 'GlobalSearch',
  components: { UserAvatar, OrgAvatar },
  setup() {
    const keyword = ref('');
    const open = ref(false);
    const appItems = ref<PluginMarketItemDto[]>([]);

    onMounted(async () => {
      try {
        await refreshOrganizations();
      } catch {
        // 组织读取失败时仍可搜索个人空间的数据
      }
      try {
        appItems.value = await window.electronAPI.pluginMarket.list();
      } catch {
        // 真实市场不可用时仍可搜索 mock 应用（mock 模式）
      }
      // 与应用页同一合并策略：仅 mock 模式追加 mock 应用
      if (mockMode()) {
        appItems.value = [...appItems.value, ...listMockApps()];
      }
    });

    // ---------------- 分组结果 ----------------

    const contactItems = computed<SearchItem[]>(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) {
        return [];
      }
      const items: SearchItem[] = [];
      for (const friend of contactsOf('personal').friends) {
        // 统一展示名入口（备注>昵称），改备注后搜索结果同步生效
        const name = personDisplayName('personal', friend.rootId);
        if (matches(kw, name, friend.nickname, friend.rootId)) {
          items.push({
            key: `friend:${friend.rootId}`,
            kind: 'contact',
            name,
            subtitle: friend.remark ? friend.nickname : '个人空间 · 朋友',
            space: { type: 'personal' },
            rootId: friend.rootId,
            avatarImage: personAvatarSource('personal', friend.rootId).image
          });
        }
      }
      for (const org of organizations.value) {
        const space: CurrentSpace = { type: 'org', orgId: org.orgId };
        for (const member of org.members) {
          // 统一组织成员入口（备注 > 组织身份昵称），种子 rootId@orgId
          const avatar = orgMemberAvatarSource(org.orgId, member.rootId, { name: shortRootId(member.rootId) });
          if (matches(kw, avatar.name, member.rootId, org.name)) {
            items.push({
              key: `member:${org.orgId}:${member.rootId}`,
              kind: 'contact',
              name: avatar.name,
              subtitle: `${org.name} · ${member.role === 'admin' ? '管理员' : '成员'}`,
              space,
              rootId: member.rootId,
              avatarSeed: avatar.seed,
              avatarImage: avatar.image
            });
          }
        }
      }
      return items.slice(0, GROUP_LIMIT);
    });

    const conversationItems = computed<SearchItem[]>(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) {
        return [];
      }
      const spaces: CurrentSpace[] = [
        { type: 'personal' },
        ...organizations.value.map((org): CurrentSpace => ({ type: 'org', orgId: org.orgId }))
      ];
      const items: SearchItem[] = [];
      for (const space of spaces) {
        for (const conv of listConversations(spaceKeyOf(space))) {
          // 会话名：direct 走统一展示名入口（备注>昵称>原标题，与 ConversationList 的 convName 同写法），
          // 改备注后搜索结果同步生效；app 走插件清单名称（缺省 pluginId）；搜索匹配也用展示名
          const name =
            conv.kind === 'direct'
              ? personDisplayName(spaceKeyOf(space), conv.peerId, conv.title)
              : conv.kind === 'app'
                ? appConversationName(conv.peerId, conv.title)
                : conv.title;
          if (matches(kw, name, conv.peerId)) {
            const orgName =
              space.type === 'org'
                ? organizations.value.find((org) => org.orgId === space.orgId)?.name ?? '组织空间'
                : '个人空间';
            items.push({
              key: `conv:${spaceKeyOf(space)}:${conv.id}`,
              kind: 'conversation',
              name,
              subtitle: orgName,
              space,
              rootId: conv.peerId,
              conversationId: conv.id,
              avatarImage: conv.kind === 'direct' ? personAvatarSource(spaceKeyOf(space), conv.peerId).image : ''
            });
          }
        }
      }
      return items.slice(0, GROUP_LIMIT);
    });

    const appResultItems = computed<SearchItem[]>(() => {
      const kw = keyword.value.trim();
      if (!kw) {
        return [];
      }
      return appItems.value
        .filter((item) => marketItemMatches(item, kw))
        .slice(0, GROUP_LIMIT)
        .map((item) => ({
          key: `app:${item.id}`,
          kind: 'app' as const,
          name: item.name,
          subtitle: item.installed ? '已安装' : '未安装',
          pluginId: item.id,
          iconBackground: appIconBackground(item)
        }));
    });

    const orgItems = computed<SearchItem[]>(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) {
        return [];
      }
      return organizations.value
        .filter((org) => matches(kw, org.name, org.description))
        .slice(0, GROUP_LIMIT)
        .map((org) => ({
          key: `org:${org.orgId}`,
          kind: 'org' as const,
          name: org.name,
          subtitle: `${org.memberCount} 名成员`,
          orgId: org.orgId
        }));
    });

    const groups = computed<SearchGroup[]>(() =>
      [
        { label: '联系人', items: contactItems.value },
        { label: '会话', items: conversationItems.value },
        { label: '应用', items: appResultItems.value },
        { label: '组织', items: orgItems.value }
      ].filter((group) => group.items.length > 0)
    );

    // ---------------- 跳转 ----------------

    const close = () => {
      open.value = false;
    };

    /** 目标在别的空间时先切空间，再派发事件（App.vue 消费并切 tab） */
    const ensureSpace = (space?: CurrentSpace) => {
      if (space && JSON.stringify(space) !== JSON.stringify(currentSpace.value)) {
        switchSpace(space);
      }
    };

    const select = (item: SearchItem) => {
      close();
      keyword.value = '';
      if (item.kind === 'contact') {
        ensureSpace(item.space);
        window.dispatchEvent(new CustomEvent('spark:open-contact', { detail: { rootId: item.rootId } }));
      } else if (item.kind === 'conversation') {
        ensureSpace(item.space);
        openChat({ rootId: item.rootId ?? '', name: item.name, conversationId: item.conversationId });
      } else if (item.kind === 'app') {
        window.dispatchEvent(new CustomEvent('spark:open-app', { detail: { id: item.pluginId } }));
      } else if (item.kind === 'org' && item.orgId) {
        switchSpace({ type: 'org', orgId: item.orgId });
      }
    };

    /** 回车选中第一个结果 */
    const pickFirst = () => {
      const first = groups.value[0]?.items[0];
      if (first) {
        select(first);
      }
    };

    return { SearchIcon: Search, keyword, open, groups, close, select, pickFirst };
  }
});
</script>

<style scoped>
.global-search {
  position: relative;
  /* 顶栏容器 min 220 / max 480（TopNavbar），这里收紧到 280-320px */
  width: clamp(280px, 100%, 320px);
  -webkit-app-region: no-drag;
}

/* 下拉面板跟随输入框宽度（同步加宽），窄窗口兜底 320px */
.global-search-dropdown {
  position: absolute;
  top: calc(100% + 6px);
  left: 50%;
  transform: translateX(-50%);
  width: 100%;
  min-width: 320px;
  max-height: 420px;
  overflow-y: auto;
  padding: 6px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-l);
  box-shadow: var(--spark-shadow-pop);
  z-index: 3000;
}

.gs-group-title {
  padding: 6px 10px 2px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
}

.gs-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  border: 0;
  background: transparent;
  cursor: pointer;
  font-family: inherit;
  padding: 7px 10px;
  border-radius: var(--spark-radius-m);
  text-align: left;
}

.gs-item:hover {
  background: var(--spark-bg-hover);
}

.gs-item-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.gs-item-name {
  font-size: var(--spark-font-size-placeholder);
  color: var(--spark-text-1);
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.gs-item-subtitle {
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.gs-app-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border-radius: var(--spark-radius-m);
  color: #fff;
  font-size: 13px;
  font-weight: 600;
  flex-shrink: 0;
}

.gs-empty {
  padding: 24px 0;
  text-align: center;
  font-size: var(--spark-font-size-placeholder);
  color: var(--spark-text-3);
}
</style>
