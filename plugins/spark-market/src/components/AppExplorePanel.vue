<!-- 探索页（移植自壳层 AppExplorePanel）：已验证全网广播（随机排序 + 换一批 +
     搜索直达）；详情对话框 resolveRepo 校正后安装。
     插件版差异：数据源与事件都走 sdk.market（announceList/announceGet/
     onAnnounceChanged，桥事件通道）；空间可见性过滤不迁移（广播条目无
     supportedSpaces 自报面，壳层用 corrected.supportedSpaces + currentSpace——
     插件实例绑定空间可从 ctx 读，过滤逻辑保留并作用于绑定空间）。 -->
<template>
  <div class="explore-panel">
    <div class="explore-toolbar">
      <el-input
        v-model="keyword"
        class="apps-search explore-search"
        placeholder="搜索名称 / 简介 / 仓库地址"
        clearable
        :prefix-icon="Search"
      />
      <el-button v-if="!searching" class="explore-shuffle" @click="reshuffle">换一批</el-button>
    </div>

    <el-alert
      v-if="pendingNewCount > 0 && !searching"
      class="explore-new-hint"
      type="success"
      :closable="false"
      show-icon
    >
      <template #title>
        有 {{ pendingNewCount }} 个新插件通过验证
        <el-button size="small" text type="success" @click="reshuffle">换一批查看</el-button>
      </template>
    </el-alert>

    <el-empty v-if="visibleEntries.length === 0" :description="emptyText" />

    <div v-else class="market-grid explore-grid">
      <div
        v-for="entry in visibleEntries"
        :key="entry.announce.id"
        class="market-card explore-card"
        @click="openDetail(entry)"
      >
        <img v-if="announceDisplayIcon(entry)" :src="announceDisplayIcon(entry)" class="market-card-icon explore-card-icon" alt="" />
        <span v-else class="market-card-icon" :style="{ background: exploreIconBackground(entry) }">
          {{ announceDisplayName(entry).slice(0, 1) }}
        </span>
        <div class="market-card-info">
          <h3>{{ announceDisplayName(entry) }} <span class="explore-card-version">v{{ announceDisplayVersion(entry) }}</span></h3>
          <p>{{ announceDisplaySummary(entry) }}</p>
          <div class="market-card-tags">
            <el-tag size="small" effect="plain">{{ announceCategoryLabel(entry.announce.category) }}</el-tag>
            <el-tag v-if="installedIds.includes(entry.announce.id)" size="small" type="success">已安装</el-tag>
          </div>
        </div>
      </div>
    </div>

    <el-dialog v-model="detailVisible" title="插件详情" width="480">
      <div v-if="detailEntry" class="repo-install-dialog">
        <div class="repo-preview-head">
          <img v-if="detailIcon" :src="detailIcon" class="repo-preview-icon" alt="" />
          <span v-else class="repo-preview-icon repo-preview-icon-fallback">{{ detailName.slice(0, 1) }}</span>
          <div>
            <h3>
              {{ detailName }}
              <el-tag size="small" effect="plain">v{{ corrected?.version ?? announceDisplayVersion(detailEntry) }}</el-tag>
            </h3>
            <p class="repo-preview-id">{{ detailEntry.announce.id }}</p>
          </div>
        </div>
        <p class="repo-preview-summary">{{ corrected?.summary ?? announceDisplaySummary(detailEntry) }}</p>
        <p class="explore-detail-meta">发布者：{{ detailEntry.announce.publisher }}</p>
        <p v-if="detailEntry.announce.releaseUrl" class="explore-detail-meta">
          发布地址：{{ detailEntry.announce.releaseUrl }}
        </p>

        <el-alert v-if="correcting" title="正在访问仓库校正声明信息…" type="info" :closable="false" show-icon />
        <el-alert v-else-if="resolveError" type="warning" :closable="false" show-icon>
          <template #title>仓库暂不可达，无法校正声明信息</template>
          <p class="explore-fallback-text">
            可自行访问仓库地址下载 .spkg 包，回到应用页用「导入 .spkg 文件」侧载安装（导入前请核对包哈希）。
          </p>
          <p class="explore-fallback-text explore-fallback-id">{{ detailEntry.announce.id }}</p>
        </el-alert>

        <p v-if="detailPermissions.length > 0" class="repo-preview-permissions">
          声明权限：{{ detailPermissions.join('、') }}
        </p>
        <p v-else-if="!correcting && !resolveError" class="repo-preview-permissions">该插件未声明额外权限</p>
      </div>
      <template #footer>
        <el-button @click="detailVisible = false">关闭</el-button>
        <el-button v-if="detailEntry && installedIds.includes(detailEntry.announce.id)" disabled>
          已安装
        </el-button>
        <el-button v-else type="primary" :disabled="!corrected" :loading="correcting" @click="confirmInstall">
          安装
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { Search } from '@element-plus/icons-vue';
import type { PluginMarketAnnounceEntry, PluginMarketRepoDeclaration } from '../../../../packages/plugin-sdk/src';
import { marketApi, pluginContext } from '../sdk-host';
import {
  announceCategoryLabel,
  announceDisplayIcon,
  announceDisplayName,
  announceDisplaySummary,
  announceDisplayVersion,
  announceMatches,
  filterVerifiedAnnounces,
  hashGradient,
  safeAnnounceIcon,
  shuffleAnnounces,
  sortAnnouncesByUpdated
} from '../market-store';

export default defineComponent({
  name: 'AppExplorePanel',
  props: {
    /** 已安装插件 id 清单（卡片「已安装」标记 + 详情安装按钮置灰） */
    installedIds: { type: Array as PropType<string[]>, default: () => [] }
  },
  emits: ['install-repo'],
  setup(props, { emit }) {
    const keyword = ref('');
    const allVerified = ref<PluginMarketAnnounceEntry[]>([]);
    const displayed = ref<PluginMarketAnnounceEntry[]>([]);

    const searching = computed(() => keyword.value.trim().length > 0);

    /** 绑定空间可见性（spaces-and-plugins §4）：corrected.supportedSpaces 缺省按 ['org'] */
    const isEntryVisibleInSpace = (entry: PluginMarketAnnounceEntry): boolean => {
      const spaces = entry.corrected?.supportedSpaces;
      const effective = spaces && spaces.length > 0 ? spaces : ['org'];
      return effective.includes(pluginContext()?.space.type ?? 'personal');
    };

    const searchResults = computed(() =>
      sortAnnouncesByUpdated(allVerified.value).filter((entry) => announceMatches(entry, keyword.value))
    );

    const visibleEntries = computed(() =>
      (searching.value ? searchResults.value : displayed.value).filter(isEntryVisibleInSpace)
    );

    const pendingNewCount = computed(() => {
      const shown = new Set(displayed.value.map((entry) => entry.announce.id));
      return allVerified.value.filter((entry) => !shown.has(entry.announce.id)).length;
    });

    const emptyText = computed(() =>
      searching.value
        ? '没有匹配的已验证插件'
        : '正在监听网络广播，还没有已验证的插件声明；开发者可在「开发者」标签页发布自己的插件'
    );

    const reshuffle = async () => {
      const market = marketApi();
      if (!market) {
        return;
      }
      try {
        const entries = await market.announceList();
        allVerified.value = filterVerifiedAnnounces(entries ?? []);
        displayed.value = shuffleAnnounces(allVerified.value);
      } catch {
        // 索引读取失败保留当前展示，不阻断浏览
      }
    };

    /** 增量更新（plugin-dist §8.7 事件）：新 verified 并入数据源但不打断当前洗牌序；
     *  已在展示序中的条目原地更新；不再 verified 的剔除 */
    const upsertVerified = async (id: string) => {
      const market = marketApi();
      if (!market) {
        return;
      }
      try {
        const entry = await market.announceGet(id);
        if (!entry || entry.verified !== 'verified') {
          allVerified.value = allVerified.value.filter((item) => item.announce.id !== id);
          displayed.value = displayed.value.filter((item) => item.announce.id !== id);
          return;
        }
        const index = allVerified.value.findIndex((item) => item.announce.id === id);
        if (index >= 0) {
          allVerified.value = allVerified.value.map((item, i) => (i === index ? entry : item));
        } else {
          allVerified.value = [...allVerified.value, entry];
        }
        const shownIndex = displayed.value.findIndex((item) => item.announce.id === id);
        if (shownIndex >= 0) {
          displayed.value = displayed.value.map((item, i) => (i === shownIndex ? entry : item));
        }
      } catch {
        // 单条查询失败等下一次换一批
      }
    };

    onMounted(async () => {
      // 先注册监听再拉索引（I7）：监听建立前发生的 verified 事件由随后的
      // 全量拉取补齐，监听建立后到达的事件经 upsert 幂等并入，不漏不重
      try {
        await marketApi()?.onAnnounceChanged((event) => {
          if (event.kind === 'verified' && event.verified) {
            void upsertVerified(event.id);
          }
        });
      } catch {
        // 事件通道不可用（纯前端预览/单测）时仅手动换一批
      }
      await reshuffle();
    });

    // 探索详情：resolveRepo 以仓库自声明校正名称/图标/权限（plugin-dist §8.8 同链路）
    const detailVisible = ref(false);
    const detailEntry = ref<PluginMarketAnnounceEntry | null>(null);
    const corrected = ref<PluginMarketRepoDeclaration | null>(null);
    const correcting = ref(false);
    const resolveError = ref('');

    const openDetail = async (entry: PluginMarketAnnounceEntry) => {
      const market = marketApi();
      if (!market) {
        return;
      }
      detailEntry.value = entry;
      corrected.value = null;
      resolveError.value = '';
      correcting.value = true;
      detailVisible.value = true;
      try {
        corrected.value = await market.resolveRepo(entry.announce.id);
      } catch (error) {
        resolveError.value = `${error}`;
      } finally {
        correcting.value = false;
      }
    };

    const detailName = computed(() =>
      corrected.value?.name ?? (detailEntry.value ? announceDisplayName(detailEntry.value) : '')
    );
    const detailIcon = computed(
      () => safeAnnounceIcon(corrected.value?.icon || '') || (detailEntry.value ? announceDisplayIcon(detailEntry.value) : '')
    );
    const detailPermissions = computed(() => corrected.value?.permissions ?? []);

    const confirmInstall = () => {
      if (!corrected.value) {
        return;
      }
      const declaration = corrected.value;
      detailVisible.value = false;
      emit('install-repo', declaration);
    };

    const exploreIconBackground = (entry: PluginMarketAnnounceEntry) =>
      hashGradient(entry.announce.id || entry.announce.name);

    return {
      keyword,
      searching,
      visibleEntries,
      pendingNewCount,
      emptyText,
      reshuffle,
      detailVisible,
      detailEntry,
      corrected,
      correcting,
      resolveError,
      detailName,
      detailIcon,
      detailPermissions,
      openDetail,
      confirmInstall,
      exploreIconBackground,
      announceCategoryLabel,
      announceDisplayIcon,
      announceDisplayName,
      announceDisplaySummary,
      announceDisplayVersion,
      Search
    };
  }
});
</script>
