<!--
  论坛插件（spark-forum）· message-card 视图：主题卡片。

  沿用 spark-example PostCard 的纪律：
  - 运行在应用消息流里的轻量 iframe 中，能力面被壳层按 view 裁剪：仅 docs
    只读 / identity.verify / evidence——卡片只做「读数据 + 验签 + 按钮回调」；
  - 卡片数据只携带引用 { topicId, orgId }，正文经 docs 查询（卡片随应用消息
    本地落库后不再更新，而主题文档会随同步演进）；
  - 按钮回调 sdk.messages.triggerCardAction 上行给壳层，经归属校验后路由给
    主视图的 onCardAction（见 ForumView.vue），卡片自身不跳转；
  - 刻意不引入 Element Plus：卡片是高频小渲染件，保持零框架依赖。
-->
<template>
  <section class="topic-card">
    <div v-if="loading" class="state">加载中…</div>
    <div v-else-if="loadError" class="state">卡片加载失败（请查看应用消息摘要）</div>
    <div v-else-if="!topic" class="state">主题不存在或尚未同步到本机</div>
    <template v-else>
      <div class="meta">
        <span v-if="boardName" class="board">{{ boardName }}</span>
        <span class="time">{{ formatDate(topic.createdAt) }}</span>
      </div>
      <p class="title">{{ topic.title }}</p>
      <p class="preview">{{ contentPreview }}</p>
      <div class="footer">
        <span class="author">{{ topic.authorRootId }}</span>
        <!-- 已签名徽标：卡片视图用免权限的 identity.verify 本地验签 -->
        <span v-if="verified" class="badge">已签名</span>
        <button class="action" type="button" @click="gotoTopic">查看/去回复</button>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import { FORUM_COLLECTIONS, ForumService } from './service';
import type { ForumBoard, ForumTopic } from './model';

export default defineComponent({
  name: 'TopicCard',
  props: {
    /** 卡片数据（应用消息 card.data 透传，由卡片入口经 props 注入） */
    cardData: {
      type: Object as PropType<{ topicId?: string; orgId?: string } | undefined>,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const loading = ref(true);
    const loadError = ref('');
    const topic = ref<ForumTopic | null>(null);
    const boardName = ref('');
    const verified = ref(false);

    let sdk: PluginSDK | null = null;

    const contentPreview = computed(() => {
      const content = topic.value?.content ?? '';
      return content.length > 120 ? `${content.slice(0, 120)}…` : content;
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
        const topicId = props.cardData?.topicId;
        if (!topicId) {
          return;
        }

        // docs 只读：卡片视图允许 docs.get / docs.query
        topic.value = await sdk.docs.get<ForumTopic>(FORUM_COLLECTIONS.topics, topicId);
        if (topic.value) {
          const board = await sdk.docs.get<ForumBoard>(FORUM_COLLECTIONS.boards, topic.value.boardId);
          boardName.value = board?.name ?? '';

          // 免权限验签：任何人可校验作者签名
          if (topic.value.signature) {
            const service = new ForumService(sdk);
            verified.value = await service.verifyTopicSignature(topic.value);
          }
        }
      } catch (error) {
        console.error('[spark-forum] topic-card 加载失败：', error);
        loadError.value = String(error);
        topic.value = null;
      } finally {
        loading.value = false;
        // 内容就绪后申请紧凑高度（壳层封顶 400px）
        requestHeight(topic.value ? 170 : 90);
      }
    });

    const gotoTopic = () => {
      // 卡片按钮回调：actionId 自定，data 捎带定位信息；
      // 壳层校验卡片归属后路由给主视图实例的 onCardAction
      sdk?.messages?.triggerCardAction('goto-topic', {
        topicId: topic.value?.id,
        orgId: props.cardData?.orgId
      });
    };

    const formatDate = (timestamp: number) =>
      new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(
        new Date(timestamp)
      );

    return { loading, loadError, topic, boardName, contentPreview, verified, gotoTopic, formatDate };
  }
});
</script>

<style scoped>
.topic-card {
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
  justify-content: space-between;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
}

.board {
  color: #0f766e;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.title {
  margin: 6px 0 0;
  font-size: 14px;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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

.author {
  color: #64748b;
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.badge {
  background: #ecfdf5;
  color: #047857;
  border: 1px solid #a7f3d0;
  border-radius: 4px;
  font-size: 11px;
  padding: 1px 6px;
}

.action {
  margin-left: auto;
  border: none;
  border-radius: 6px;
  background: #0f766e;
  color: #fff;
  font-size: 12px;
  padding: 4px 12px;
  cursor: pointer;
}

.action:hover {
  background: #115e59;
}
</style>
