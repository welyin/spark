<!--
  任务看板插件（spark-kanban）· message-card 视图：看板动态卡片（被指派通知）。

  沿用 spark-forum TopicCard 的纪律：
  - 运行在应用消息流里的轻量 iframe 中，能力面被壳层按 view 裁剪——卡片只做
    「读数据 + 验签 + 按钮回调」；
  - 卡片数据只携带引用 { cardId, boardId, orgId }，标题经 sdk.data 查询
    （卡片随应用消息本地落库后不再更新，而看板数据会随同步演进）；
  - 按钮回调 sdk.messages.triggerCardAction 上行给壳层，经归属校验后路由给
    主视图的 onCardAction（见 KanbanView.vue），卡片自身不跳转；
  - 刻意不引入 Element Plus：卡片是高频小渲染件，保持零框架依赖。
-->
<template>
  <section class="kanban-card-notify">
    <div v-if="loading" class="state">加载中…</div>
    <div v-else-if="loadError" class="state">卡片加载失败（请查看应用消息摘要）</div>
    <div v-else-if="!cardTitle" class="state">卡片不存在或尚未同步到本机</div>
    <template v-else>
      <div class="meta">
        <span v-if="boardName" class="board">{{ boardName }}</span>
        <span class="kind">被指派</span>
      </div>
      <p class="title">{{ cardTitle }}</p>
      <div class="footer">
        <span v-if="verified" class="badge">已签名</span>
        <button class="action" type="button" @click="gotoCard">查看卡片</button>
      </div>
    </template>
  </section>
</template>

<script lang="ts">
import { defineComponent, onMounted, ref, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import { buildKanbanSignPayload, cardOpSignContent, type KanbanBoard, type KanbanCardOp } from './model';
import { KANBAN_COLLECTIONS } from './service';

export default defineComponent({
  name: 'KanbanCard',
  props: {
    /** 卡片数据（应用消息 card.data 透传，由卡片入口经 props 注入） */
    cardData: {
      type: Object as PropType<{ cardId?: string; boardId?: string; orgId?: string } | undefined>,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const loading = ref(true);
    const loadError = ref('');
    const cardTitle = ref('');
    const boardName = ref('');
    const verified = ref(false);

    let sdk: PluginSDK | null = null;

    onMounted(async () => {
      // 失败即降级：握手失败/超时由壳层宿主降级为原生摘要；视图内运行期异常
      // 落本组件的错误占位——绝不卡在永久「加载中」。
      try {
        sdk = await ensurePluginSDK();
        const { cardId, boardId, orgId } = props.cardData ?? {};
        if (!cardId || !boardId || !orgId) {
          loadError.value = '卡片数据缺少引用字段';
          return;
        }
        const [board, ops] = await Promise.all([
          sdk.data.get<KanbanBoard>(KANBAN_COLLECTIONS.boards, `${orgId}/${boardId}`),
          sdk.data.query<KanbanCardOp>(KANBAN_COLLECTIONS.cardOps, { prefix: `${orgId}/`, limit: 2000 })
        ]);
        boardName.value = board?.name ?? '';
        const createOp = ops.items
          .map((item) => item.value)
          .filter((op) => op && op.cardId === cardId && op.kind === 'create')
          .sort((a, b) => a.createdAt - b.createdAt)[0];
        if (!createOp) {
          loadError.value = '卡片不存在或尚未同步到本机';
          return;
        }
        cardTitle.value = createOp.title ?? cardId;
        // 已签名徽标：卡片视图用免权限的 identity.verify 本地验签（重算载荷比对）
        if (createOp.signature) {
          try {
            const expected = buildKanbanSignPayload(createOp.orgId, createOp.id, createOp.operatorRootId, cardOpSignContent(createOp));
            if (createOp.signature.payload === expected) {
              const result = await sdk.identity.verify(expected, createOp.signature.signature, createOp.signature.publicKey);
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
          sdk?.messages?.requestCardHeight(140);
        } catch {
          /* 非卡片上下文静默忽略 */
        }
      }
    });

    const gotoCard = () => {
      // 按钮回调上行壳层 → 主视图 onCardAction（归属校验在壳层）
      sdk?.messages?.triggerCardAction('goto-card', props.cardData);
    };

    return { loading, loadError, cardTitle, boardName, verified, gotoCard };
  }
});
</script>

<style scoped>
.kanban-card-notify {
  font-family: inherit;
  padding: 4px 2px;
}

.state {
  color: #64748b;
  font-size: 13px;
}

.meta {
  display: flex;
  justify-content: space-between;
  color: #64748b;
  font-size: 12px;
}

.board {
  color: #0f766e;
  font-weight: 600;
}

.kind {
  color: #b45309;
}

.title {
  margin: 6px 0;
  font-weight: 600;
  word-break: break-word;
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
