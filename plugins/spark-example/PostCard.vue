<!--
  示例插件（spark-example）· message-card 视图：帖子卡片。

  教学要点：
  - 本视图运行在应用消息流里的轻量 iframe 中（宿主 AppMessageCard.vue），
    能力面被壳层按 view 裁剪：仅 docs 只读 / identity.verify / evidence，
    无网络、无签名、无应用消息写权限——所以卡片只做「读数据 + 验签 +
    按钮回调」三件事；
  - 卡片数据（ctx.mount.cardData）只携带引用 { postId }，正文经 docs
    查询——应用消息本地落库后不再更新，而帖子文档会随同步演进；
  - 按钮回调 sdk.messages.triggerCardAction 上行给壳层，经归属校验后
    路由给主视图的 onCardAction（见 ExampleView.vue），卡片自身不跳转；
  - 高度经 sdk.messages.requestCardHeight 申请，壳层封顶 400px；
  - 刻意不引入 Element Plus：卡片是高频小渲染件，保持零框架依赖的
    轻量样式（主视图才用完整组件库）。
-->
<template>
  <section class="post-card">
    <div v-if="loading" class="state">加载中…</div>
    <div v-else-if="loadError" class="state">卡片加载失败（请查看应用消息摘要）</div>
    <div v-else-if="!post" class="state">帖子不存在或尚未同步到本机</div>
    <template v-else>
      <div class="meta">
        <strong class="author">{{ post.authorRootId }}</strong>
        <span class="time">{{ formatDate(post.createdAt) }}</span>
      </div>
      <p class="content">{{ post.content }}</p>
      <div class="footer">
        <span class="comments">评论 {{ commentCount }}</span>
        <!-- 已签名徽标：卡片视图用免权限的 identity.verify 本地验签 -->
        <span v-if="verified" class="badge">已签名</span>
        <button class="action" type="button" @click="gotoComments">去评论</button>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import { WEIBO_COLLECTIONS, WeiboService } from './service';
import type { WeiboComment, WeiboPost } from './model';

export default defineComponent({
  name: 'PostCard',
  props: {
    /** 卡片数据（应用消息 card.data 透传，由卡片入口经 props 注入） */
    cardData: {
      type: Object as PropType<{ postId?: string } | undefined>,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const loading = ref(true);
    const loadError = ref('');
    const post = ref<WeiboPost | null>(null);
    const commentCount = ref(0);
    const verified = ref(false);

    let sdk: PluginSDK | null = null;

    const requestHeight = (height: number) => {
      // 仅 message-card 视图可用（主视图调用会抛错），此处防御性 try/catch
      try {
        sdk?.messages?.requestCardHeight(height);
      } catch {
        /* 非卡片上下文静默忽略 */
      }
    };

    onMounted(async () => {
      // 失败即降级：握手失败/超时由壳层宿主（AppMessageCard）emit('fallback')
      // 降级为原生摘要；视图内运行期异常（docs 读取失败等）则落本组件的
      // 错误占位——与「未装插件也能读懂摘要」的可达性原则一致，绝不卡在
      // 永久「加载中」。
      try {
        sdk = await ensurePluginSDK();
        const postId = props.cardData?.postId;
        if (!postId) {
          return;
        }

        // docs 只读：卡片视图允许 docs.get / docs.query
        post.value = await sdk.docs.get<WeiboPost>(WEIBO_COLLECTIONS.posts, postId);
        if (post.value) {
          const comments = await sdk.docs.query<WeiboComment>(WEIBO_COLLECTIONS.comments, {
            filter: [{ field: 'postId', value: postId }],
            limit: 500
          });
          commentCount.value = comments.items.length;

          // 免权限验签：任何人可校验作者签名（防抵赖演示的另一半）
          if (post.value.signature) {
            const service = new WeiboService(sdk);
            verified.value = await service.verifyPostSignature(post.value);
          }
        }
      } catch (error) {
        console.error('[spark-example] post-card 加载失败：', error);
        loadError.value = String(error);
        post.value = null;
      } finally {
        loading.value = false;
        // 内容就绪后申请紧凑高度（壳层封顶 400px）
        requestHeight(post.value ? 150 : 90);
      }
    });

    const gotoComments = () => {
      // 卡片按钮回调：actionId 自定，data 捎带定位信息；
      // 壳层校验卡片归属后路由给主视图实例的 onCardAction
      sdk?.messages?.triggerCardAction('goto-comments', { postId: post.value?.id });
    };

    const formatDate = (timestamp: number) =>
      new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(
        new Date(timestamp)
      );

    return { loading, loadError, post, commentCount, verified, gotoComments, formatDate };
  }
});
</script>

<style scoped>
.post-card {
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

.author {
  color: #0f766e;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.content {
  margin: 8px 0;
  white-space: pre-wrap;
  word-break: break-word;
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.footer {
  display: flex;
  align-items: center;
  gap: 8px;
}

.comments {
  color: #64748b;
  font-size: 12px;
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
