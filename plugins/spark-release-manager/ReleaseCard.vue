<!--
  发布管理插件（spark-release-manager）· message-card 视图：发布卡片。

  沿用 spark-kanban KanbanCard 的纪律：
  - 运行在应用消息流里的轻量 iframe 中，能力面被壳层按 view 裁剪——卡片只做
    「读数据 + 验签 + 按钮回调」；
  - 卡片数据只携带引用 { releaseId, orgId }（releaseRef = 发布单记录 id，
    档三-24），发布单与状态经 sdk.docs 查询（卡片随应用消息本地落库后不再
    更新，而发布数据会随同步演进——已撤回的发布单卡片实时标注「已撤回」）；
  - 按钮回调 sdk.messages.triggerCardAction 上行给壳层，经归属校验后路由给
    主视图的 onCardAction（见 ReleaseManagerView.vue），卡片自身不跳转；
  - 刻意不引入 Element Plus：卡片是高频小渲染件，保持零框架依赖。
-->
<template>
  <section class="release-card">
    <div v-if="loading" class="state">加载中…</div>
    <div v-else-if="loadError" class="state">卡片加载失败（请查看应用消息摘要）</div>
    <template v-else>
      <div class="meta">
        <span class="plugin">{{ pluginName }}</span>
        <span class="version">v{{ release?.version }}</span>
        <span v-if="retracted" class="kind retracted">已撤回</span>
        <span v-else class="kind published">版本发布</span>
      </div>
      <p class="title">{{ changelogPreview }}</p>
      <div class="footer">
        <span v-if="verified" class="badge">已签名</span>
        <span v-else class="badge dim">{{ publisherShort }}</span>
        <button class="action" type="button" @click="gotoRelease">查看发布单</button>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildReleaseSignPayload,
  deriveReleaseState,
  filterAuthorizedReleaseEvents,
  pluginDisplayName,
  releaseEventOperatorSet,
  releaseSignContent,
  type ReleaseEvent,
  type ReleaseManagerConfig,
  type ReleaseRecord
} from './model';
import { RELEASE_COLLECTIONS } from './service';

export default defineComponent({
  name: 'ReleaseCard',
  props: {
    /** 卡片数据（应用消息 card.data 透传，由卡片入口经 props 注入） */
    cardData: {
      type: Object as PropType<{ releaseId?: string; orgId?: string } | undefined>,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const loading = ref(true);
    const loadError = ref('');
    const release = ref<ReleaseRecord | null>(null);
    const retracted = ref(false);
    const verified = ref(false);

    let sdk: PluginSDK | null = null;

    const pluginName = computed(() => (release.value ? pluginDisplayName(release.value.pluginId) : ''));
    const publisherShort = computed(() =>
      release.value ? `发布者 ${release.value.publisherRootId.slice(0, 12)}…` : ''
    );
    const changelogPreview = computed(() => {
      const text = (release.value?.changelog ?? '').trim().replace(/\s+/g, ' ');
      return text || '（无变更说明）';
    });

    onMounted(async () => {
      // 失败即降级：握手失败/超时由壳层宿主降级为原生摘要；视图内运行期异常
      // 落本组件的错误占位——绝不卡在永久「加载中」。
      try {
        sdk = await ensurePluginSDK();
        const { releaseId, orgId } = props.cardData ?? {};
        if (!releaseId || !orgId) {
          loadError.value = '卡片数据缺少引用字段';
          return;
        }
        const [record, eventsResponse, releaseConfig] = await Promise.all([
          sdk.docs.get<ReleaseRecord>(RELEASE_COLLECTIONS.releases, releaseId),
          sdk.docs.query<ReleaseEvent>(RELEASE_COLLECTIONS.events, {
            filter: [{ field: 'orgId', value: orgId }],
            limit: 2000
          }),
          // 读侧鉴权 fail-closed（伪造状态事件不得改写卡片标注）：合法操作者集合
          // = 发布权配置登记的 publisherRootIds（经 docs 读 release_config）；
          // 配置不可得时宁可不标注「已撤回」。卡片视图无名册上下文，管理员操作
          // 在卡片上可能漏标，详情页按完整口径（发布权集合 ∪ 名册管理员）兜底。
          sdk.docs.get<ReleaseManagerConfig>(RELEASE_COLLECTIONS.config, orgId)
        ]);
        if (!record || record.orgId !== orgId) {
          loadError.value = '发布单不存在或尚未同步到本机';
          return;
        }
        release.value = record;
        if (releaseConfig) {
          const authorized = filterAuthorizedReleaseEvents(
            eventsResponse.items.map((item) => item.data),
            releaseEventOperatorSet(releaseConfig)
          );
          retracted.value = deriveReleaseState(record.id, authorized) === 'retracted';
        }
        // 已签名徽标：卡片视图用免权限的 identity.verify 本地验签（重算载荷比对）
        if (record.signature) {
          try {
            const expected = buildReleaseSignPayload(
              record.orgId,
              record.id,
              record.publisherRootId,
              releaseSignContent(record)
            );
            if (record.signature.payload === expected) {
              const result = await sdk.identity.verify(expected, record.signature.signature, record.signature.publicKey);
              verified.value = result.valid;
            }
          } catch {
            verified.value = false;
          }
        }
      } catch (error) {
        loadError.value = String(error);
      } finally {
        loading.value = false;
        try {
          sdk?.messages?.requestCardHeight(150);
        } catch {
          /* 非卡片上下文静默忽略 */
        }
      }
    });

    const gotoRelease = () => {
      // 按钮回调上行壳层 → 主视图 onCardAction（归属校验在壳层）
      sdk?.messages?.triggerCardAction('goto-release', props.cardData);
    };

    return { loading, loadError, release, retracted, verified, pluginName, publisherShort, changelogPreview, gotoRelease };
  }
});
</script>

<style scoped>
.release-card {
  font-family: inherit;
  padding: 4px 2px;
}

.state {
  color: #64748b;
  font-size: 13px;
}

.meta {
  display: flex;
  gap: 8px;
  align-items: center;
  color: #64748b;
  font-size: 12px;
}

.plugin {
  color: #0f766e;
  font-weight: 600;
}

.version {
  font-variant-numeric: tabular-nums;
}

.kind.published {
  color: #0f766e;
}

.kind.retracted {
  color: #b91c1c;
}

.title {
  margin: 6px 0;
  font-weight: 600;
  word-break: break-word;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 6px;
}

.badge {
  color: #16a34a;
  font-size: 12px;
}

.badge.dim {
  color: #94a3b8;
}

.action {
  border: 1px solid #0f766e;
  color: #0f766e;
  background: transparent;
  border-radius: 6px;
  padding: 2px 10px;
  font-size: 12px;
  cursor: pointer;
}
</style>
