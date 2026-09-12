<!-- 顶栏全局搜索（纯前端模糊匹配）：分组展示 联系人/会话/应用/组织，
     点击结果跳转到目标页面（联系人资料 / 会话 / 应用详情 / 切换组织空间）。
     数据源全部复用现有 store/接口：mock/contacts + org-membership（联系人）、
     mock/messages（会话）、pluginMarket.list + mock/apps（应用）、org-membership（组织） -->
<template>
  <div class="global-search">
    <el-input
      ref="inputRef"
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
    <div
      v-if="open && keyword.trim()"
      class="global-search-dropdown"
      @mousedown.prevent
    >
      <!-- G1 加载态：索引（组织/应用清单）尚未就绪时如实提示，而非误报「无匹配」 -->
      <div v-if="loading" class="gs-empty">索引加载中…</div>
      <template v-else-if="groups.length > 0">
        <div v-for="group in groups" :key="group.label" class="gs-group">
          <!-- G1：每组标明索引源（只搜元数据面，不搜内容） -->
          <div class="gs-group-title">
            {{ group.label
            }}<span class="gs-group-source">{{ group.source }}</span>
          </div>
          <button
            v-for="item in group.items"
            :key="item.key"
            type="button"
            class="gs-item"
            @click="select(item)"
          >
            <UserAvatar
              v-if="item.kind === 'contact' || item.kind === 'conversation'"
              :root-id="item.avatarSeed ?? item.rootId ?? ''"
              :nickname="item.name"
              :avatar="item.avatarImage ?? ''"
              :size="28"
            />
            <OrgAvatar
              v-else-if="item.kind === 'org'"
              :org-id="item.orgId ?? ''"
              :name="item.name"
              :size="28"
            />
            <AppIcon
              v-else-if="item.kind === 'app'"
              class="gs-app-icon"
              :item="item.app ?? null"
            />
            <span
              v-else
              class="gs-app-icon"
              :style="{ background: item.iconBackground }"
              >{{ item.name.slice(0, 1) }}</span
            >
            <span class="gs-item-main">
              <span class="gs-item-name">{{ item.name }}</span>
              <span class="gs-item-subtitle">{{ item.subtitle }}</span>
            </span>
          </button>
        </div>
      </template>
      <!-- G1 空态：如实列出已检索的索引面与未覆盖面（文件内容索引暂无数据源，不虚构） -->
      <div v-else class="gs-empty">
        <p class="gs-empty-title">无匹配结果</p>
        <p class="gs-empty-note">
          已检索本机索引：联系人 / 会话元数据 / 应用清单 / 组织 / 事务元数据。
        </p>
        <p class="gs-empty-note">文件内容索引尚未建立（暂无数据源）。</p>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { Search } from '@element-plus/icons-vue';
import type { PluginMarketItemDto } from '../api/types';
import {
  currentSpace,
  switchSpace,
  type CurrentSpace,
} from '../stores/current-space';
import { organizations, refreshOrganizations } from '../stores/org-membership';
import { personalSpaceName } from '../stores/personal-space';
import {
  orgMemberAvatarSource,
  personAvatarSource,
  personDisplayName,
} from '../stores/avatar-sources';
import { contactsOf } from '../mock/contacts';
import { listConversations, spaceKeyOf } from '../stores/messages';
import { appConversationName } from '../stores/app-conversations';
import { listMockApps } from '../mock/apps';
import { mockMode } from '../mock/mode';
import { marketItemMatches } from './apps/apps-store';
import { isPluginVisibleInSpace } from './apps/space-visibility';
import AppIcon from './apps/AppIcon.vue';
import { openChat } from './contacts/open-intents';
import { openPluginDeepLink } from '../services/deep-link';
import {
  affairFeed,
  refreshAffairFeed,
  type AffairFeedItem,
} from '../stores/affairs/affair-feed';
import { openAffairInPlugin } from '../stores/affairs/affair-open';
import UserAvatar from './UserAvatar.vue';
import OrgAvatar from './OrgAvatar.vue';

/** 每组最多展示的条数 */
const GROUP_LIMIT = 5;

type SearchItem = {
  key: string;
  kind: 'contact' | 'conversation' | 'app' | 'org' | 'affair';
  name: string;
  subtitle: string;
  /** 联系人/会话所属空间（跳转前先切空间） */
  space?: CurrentSpace;
  rootId?: string;
  conversationId?: string;
  pluginId?: string;
  orgId?: string;
  /** 事务条目：打开时分发到类型插件 */
  affair?: AffairFeedItem;
  /** 应用条目：完整市场条目（AppIcon 回退链消费，plugin-dist §2.3） */
  app?: PluginMarketItemDto;
  iconBackground?: string;
  /** 头像配色种子：组织成员=rootId@orgId；缺省=rootId */
  avatarSeed?: string;
  /** 已上传的头像图片（dataURL）；空/缺省=自动配色头像 */
  avatarImage?: string;
};

type SearchGroup = { label: string; source: string; items: SearchItem[] };

const shortRootId = (rootId: string) => `${rootId.slice(0, 10)}...`;
const matches = (keyword: string, ...fields: string[]) =>
  fields.join('\n').toLowerCase().includes(keyword);

export default defineComponent({
  name: 'GlobalSearch',
  components: { UserAvatar, OrgAvatar, AppIcon },
  // select：选中任一搜索结果后触发（移动端全屏搜索层借此关闭自身；桌面端无监听，行为不变）
  emits: ['select'],
  setup(_, { emit }) {
    const keyword = ref('');
    const open = ref(false);
    const appItems = ref<PluginMarketItemDto[]>([]);
    /** G1 加载态：组织/应用清单/事务索引任一未就绪即视为加载中 */
    const loading = ref(true);
    /** 输入框引用：移动端全屏搜索层打开时主动聚焦（弹键盘） */
    const inputRef = ref<{ focus: () => void } | null>(null);
    const focusInput = () => inputRef.value?.focus();

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
      // 事务元数据索引（与我相关列表）：失败不阻断其余分组
      try {
        await refreshAffairFeed();
      } catch {
        // 事务接口不可用时事务组为空
      }
      loading.value = false;
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
            subtitle: friend.remark ? friend.nickname : `${personalSpaceName.value} · 朋友`,
            space: { type: 'personal' },
            rootId: friend.rootId,
            avatarImage: personAvatarSource('personal', friend.rootId).image,
          });
        }
      }
      for (const org of organizations.value) {
        const space: CurrentSpace = { type: 'org', orgId: org.orgId };
        for (const member of org.members) {
          // 统一组织成员入口（备注 > 组织身份昵称），种子 rootId@orgId
          const avatar = orgMemberAvatarSource(org.orgId, member.rootId, {
            name: shortRootId(member.rootId),
          });
          if (matches(kw, avatar.name, member.rootId, org.name)) {
            items.push({
              key: `member:${org.orgId}:${member.rootId}`,
              kind: 'contact',
              name: avatar.name,
              subtitle: `${org.name} · ${member.role === 'admin' ? '管理员' : '成员'}`,
              space,
              rootId: member.rootId,
              avatarSeed: avatar.seed,
              avatarImage: avatar.image,
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
        ...organizations.value.map((org): CurrentSpace => ({
          type: 'org',
          orgId: org.orgId,
        })),
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
                ? (organizations.value.find((org) => org.orgId === space.orgId)
                    ?.name ?? '组织空间')
                : personalSpaceName.value;
            items.push({
              key: `conv:${spaceKeyOf(space)}:${conv.id}`,
              kind: 'conversation',
              name,
              subtitle: orgName,
              space,
              rootId: conv.peerId,
              conversationId: conv.id,
              avatarImage:
                conv.kind === 'direct'
                  ? personAvatarSource(spaceKeyOf(space), conv.peerId).image
                  : '',
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
      return (
        appItems.value
          .filter((item) => marketItemMatches(item, kw))
          // 与应用页同口径按当前空间过滤（spaces-and-plugins §4）：缺省按 ['org']
          .filter((item) =>
            isPluginVisibleInSpace(
              item.supportedSpaces,
              currentSpace.value.type,
            ),
          )
          .slice(0, GROUP_LIMIT)
          .map((item) => ({
            key: `app:${item.id}`,
            kind: 'app' as const,
            name: item.name,
            subtitle: item.installed ? '已安装' : '未安装',
            pluginId: item.id,
            app: item,
          }))
      );
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
          orgId: org.orgId,
        }));
    });

    /** G1 事务类目：affair-feed 本机缓存的元数据面（标题/摘要/标签），不搜内容 */
    const affairItems = computed<SearchItem[]>(() => {
      const kw = keyword.value.trim().toLowerCase();
      if (!kw) {
        return [];
      }
      return affairFeed.value
        .filter((item) =>
          matches(kw, item.title, item.summary, item.tags.join(' ')),
        )
        .slice(0, GROUP_LIMIT)
        .map((item) => ({
          key: `affair:${item.affairId}`,
          kind: 'affair' as const,
          name: item.title,
          subtitle: item.closed ? '事务 · 已关闭' : '事务 · 进行中',
          affair: item,
          iconBackground: '#64748b',
        }));
    });

    const groups = computed<SearchGroup[]>(() =>
      [
        {
          label: '联系人',
          source: '本机索引 · 通讯录元数据',
          items: contactItems.value,
        },
        {
          label: '会话',
          source: '本机索引 · 会话元数据',
          items: conversationItems.value,
        },
        // G1 事务类目：只搜元数据面（标题/摘要/标签），点击按类型分发到承接插件
        {
          label: '事务',
          source: '本机索引 · 事务元数据',
          items: affairItems.value,
        },
        {
          label: '应用',
          source: '本机索引 · 应用清单',
          items: appResultItems.value,
        },
        // G1「域」类目：组织空间即域
        {
          label: '组织（域）',
          source: '本机索引 · 组织列表',
          items: orgItems.value,
        },
      ].filter((group) => group.items.length > 0),
    );

    // ---------------- 跳转 ----------------

    const close = () => {
      open.value = false;
    };

    /** 目标在别的空间时先切空间，再派发事件（App.vue 消费并切 tab） */
    const ensureSpace = (space?: CurrentSpace) => {
      if (
        space &&
        JSON.stringify(space) !== JSON.stringify(currentSpace.value)
      ) {
        switchSpace(space);
      }
    };

    const select = (item: SearchItem) => {
      close();
      keyword.value = '';
      emit('select');
      if (item.kind === 'contact') {
        ensureSpace(item.space);
        // 0.3：通讯录是空间插件，经统一深链打开并定位该联系人（cardData.rootId 注入插件消费）
        openPluginDeepLink({
          pluginId: 'spark-contacts',
          cardData: { rootId: item.rootId },
          space: item.space,
        });
      } else if (item.kind === 'conversation') {
        ensureSpace(item.space);
        openChat({
          rootId: item.rootId ?? '',
          name: item.name,
          conversationId: item.conversationId,
        });
      } else if (item.kind === 'app') {
        window.dispatchEvent(
          new CustomEvent('spark:open-app', { detail: { id: item.pluginId } }),
        );
      } else if (item.kind === 'affair' && item.affair) {
        // 事务条目：按类型分发到承接插件（同事务页点卡片）；未装承接插件如实提示
        void openAffairInPlugin(item.affair).then((result) => {
          if (!result.ok) {
            ElMessage.warning(
              '本机未安装能处理该事务的应用，可前往应用市场安装',
            );
          }
        });
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

    return {
      SearchIcon: Search,
      keyword,
      open,
      loading,
      groups,
      close,
      select,
      pickFirst,
      inputRef,
      focusInput,
    };
  },
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
  z-index: var(--spark-z-search);
}

.gs-group-title {
  padding: 6px 10px 2px;
  font-size: var(--spark-font-size-secondary);
  color: var(--spark-text-3);
  display: flex;
  align-items: baseline;
  gap: 6px;
}

/* G1 索引源标注：弱于组名一级 */
.gs-group-source {
  font-size: 11px;
  color: var(--spark-text-3);
  opacity: 0.8;
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

/* G1 空态注记：列出已检索索引面与未覆盖面 */
.gs-empty-title {
  margin: 0 0 6px;
}

.gs-empty-note {
  margin: 0;
  padding: 0 24px;
  font-size: var(--spark-font-size-secondary);
  line-height: 1.6;
}
</style>
