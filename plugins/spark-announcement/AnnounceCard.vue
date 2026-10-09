<!--
  公告通知插件（spark-announcement）· message-card 视图：公告卡片。

  沿用 spark-forum TopicCard 的纪律：
  - 运行在应用消息流里的轻量 iframe 中，能力面被壳层按 view 裁剪：仅 docs
    只读 / identity.verify——卡片只做「读数据 + 验签 + 按钮回调」；
  - 卡片数据只携带引用 { announcementId, orgId }，正文与撤回状态经 docs
    查询（卡片随应用消息本地落库后不再更新，而撤回记录会随同步演进——
    撤回后已送达卡片标注「已撤回」，不伪造「从未发布」）；
  - 撤回标注的读侧鉴权（U1 修复，fail-closed）：经 docs 读 announcement_config
    拿发布权集合 publisherRootIds 过滤撤回人，伪造撤回记录（非发布权集合成员）
    不参与派生；配置不可得时宁可不标注——公告详情页按「发布权集合 ∪ 名册
    管理员」的完整口径判定，是权威兜底；
  - 按钮回调 sdk.messages.triggerCardAction 上行给壳层，经归属校验后路由给
    主视图的 onCardAction（见 AnnouncementView.vue），卡片自身不跳转；
  - 刻意不引入 Element Plus：卡片是高频小渲染件，保持零框架依赖。
-->
<template>
  <section class="announce-card">
    <div v-if="loading" class="state">加载中…</div>
    <div v-else-if="loadError" class="state">卡片加载失败（请查看应用消息摘要）</div>
    <div v-else-if="!announcement" class="state">公告不存在或尚未同步到本机</div>
    <template v-else>
      <div class="meta">
        <span class="kind" :class="announcement.kind">
          {{ announcement.kind === 'release' ? '版本公告' : '团队通知' }}
        </span>
        <span v-if="announcement.version" class="version">{{ announcement.version }}</span>
        <span class="time">{{ formatDate(announcement.publishedAt) }}</span>
      </div>
      <p class="title" :class="{ retracted: retraction }">{{ announcement.title }}</p>
      <p class="preview">{{ bodyPreview }}</p>
      <div class="footer">
        <span v-if="retraction" class="badge retracted-badge">已撤回</span>
        <!-- 已签名徽标：卡片视图用免权限的 identity.verify 本地验签 -->
        <span v-else-if="verified" class="badge">已签名</span>
        <button class="action" type="button" @click="viewDetail">查看全文</button>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import { ANNOUNCEMENT_COLLECTIONS, AnnouncementService } from './service';
import { deriveRetractionMap } from './model';
import type { Announcement, AnnouncementConfig, AnnouncementRetraction } from './model';

export default defineComponent({
  name: 'AnnounceCard',
  props: {
    /** 卡片数据（应用消息 card.data 透传，由卡片入口经 props 注入） */
    cardData: {
      type: Object as PropType<{ announcementId?: string; orgId?: string } | undefined>,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const loading = ref(true);
    const loadError = ref('');
    const announcement = ref<Announcement | null>(null);
    const retraction = ref<AnnouncementRetraction | null>(null);
    const verified = ref(false);

    let sdk: PluginSDK | null = null;

    const bodyPreview = computed(() => {
      const body = announcement.value?.body.replace(/\s+/g, ' ').trim() ?? '';
      return body.length > 120 ? `${body.slice(0, 120)}…` : body;
    });

    const requestHeight = (height: number) => {
      // 仅 message-card 视图可用（主视图调用会抛错），此处防御性 try/catch
      try {
        sdk?.messages?.requestCardHeight(height);
      } catch {
        /* 非卡片上下文静默忽略 */
      }
    };

    onMounted(async () => {
      // 失败即降级：握手失败/超时由壳层宿主降级为原生摘要；视图内运行期异常
      // 落本组件的错误占位——绝不卡在永久「加载中」。
      try {
        sdk = await ensurePluginSDK();
        const announcementId = props.cardData?.announcementId;
        if (!announcementId) {
          return;
        }

        // docs 只读：卡片视图允许 docs.get / docs.query
        announcement.value = await sdk.docs.get<Announcement>(ANNOUNCEMENT_COLLECTIONS.items, announcementId);
        if (announcement.value) {
          // 撤回状态实时查询（卡片落库后不更新，撤回记录随同步演进）。
          // 读侧鉴权 fail-closed（U1）：合法撤回人集合 = 发布权配置登记的
          // publisherRootIds（经 docs 读 announcement_config）；配置不可得时
          // 宁可不标注——详情页按完整口径（发布权集合 ∪ 名册管理员）权威兜底。
          // 卡片视图无名册上下文，管理员撤回在卡片上可能漏标，详情页为准。
          const config = await sdk.docs.get<AnnouncementConfig>(
            ANNOUNCEMENT_COLLECTIONS.config,
            announcement.value.orgId
          );
          if (config) {
            const retractions = await sdk.docs.query<AnnouncementRetraction>(
              ANNOUNCEMENT_COLLECTIONS.retractions,
              { filter: [{ field: 'orgId', value: announcement.value.orgId }], limit: 1000 }
            );
            const map = deriveRetractionMap(
              retractions.items.map((item) => item.data),
              new Set(config.publisherRootIds)
            );
            retraction.value = map.get(announcementId) ?? null;
          }

          // 免权限验签：任何人可校验发布者签名
          if (announcement.value.signature) {
            const service = new AnnouncementService(sdk);
            verified.value = await service.verifyAnnouncementSignature(announcement.value);
          }
        }
      } catch (error) {
        console.error('[spark-announcement] announce-card 加载失败：', error);
        loadError.value = String(error);
        announcement.value = null;
      } finally {
        loading.value = false;
        // 内容就绪后申请紧凑高度（壳层封顶 400px）
        requestHeight(announcement.value ? 170 : 90);
      }
    });

    const viewDetail = () => {
      // 卡片按钮回调：actionId 自定，data 捎带定位信息；
      // 壳层校验卡片归属后路由给主视图实例的 onCardAction
      sdk?.messages?.triggerCardAction('view-announcement', {
        announcementId: announcement.value?.id,
        orgId: props.cardData?.orgId
      });
    };

    const formatDate = (timestamp: number) =>
      new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(
        new Date(timestamp)
      );

    return { loading, loadError, announcement, retraction, bodyPreview, verified, viewDetail, formatDate };
  }
});
</script>

<style scoped>
.announce-card {
  padding: 12px 14px;
  font-size: 13px;
  color: #1e293b;
}

.state {
  color: #64748b;
  text-align: center;
  padding: 16px 0;
}

.meta {
  display: flex;
  align-items: center;
  gap: 8px;
  color: #64748b;
  font-size: 12px;
}

.kind {
  font-weight: 600;
}

.kind.release {
  color: #b45309;
}

.kind.notice {
  color: #0f766e;
}

.version {
  background: #fef3c7;
  color: #92400e;
  border: 1px solid #fde68a;
  border-radius: 4px;
  font-size: 11px;
  padding: 1px 6px;
}

.time {
  margin-left: auto;
}

.title {
  margin: 6px 0 0;
  font-size: 14px;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.title.retracted {
  color: #94a3b8;
  text-decoration: line-through;
}

.preview {
  margin: 6px 0;
  color: #475569;
  white-space: pre-wrap;
  word-break: break-word;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.footer {
  display: flex;
  align-items: center;
  gap: 8px;
}

.badge {
  background: #ecfdf5;
  color: #047857;
  border: 1px solid #a7f3d0;
  border-radius: 4px;
  font-size: 11px;
  padding: 1px 6px;
}

.retracted-badge {
  background: #f1f5f9;
  color: #64748b;
  border-color: #cbd5e1;
}

.action {
  margin-left: auto;
  border: none;
  border-radius: 6px;
  background: #b45309;
  color: #fff;
  font-size: 12px;
  padding: 4px 12px;
  cursor: pointer;
}

.action:hover {
  background: #92400e;
}
</style>
