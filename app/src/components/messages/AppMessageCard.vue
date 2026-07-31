<!-- 应用消息卡片宿主（message-card 轻量 iframe 宿主，设计文档「UI 集成点」）。
     与 PluginIframeHost 的差异：无五态覆盖层/心跳熔断——加载失败直接 emit('fallback')
     由上层降级为原生摘要；高度由插件经桥申请（requestCardHeight），壳层封顶 400px；
     能力面按 view 裁剪（dispatcher viewType='message-card'：无网络、无签名、无 messages 写）。
     卡片按钮回调经桥 action 上行 → plugin-card-actions 归属校验 → 主视图实例 onCardAction。 -->
<template>
  <div class="app-message-card" :style="{ height: `${height}px` }">
    <iframe
      v-if="status !== 'failed'"
      ref="iframeEl"
      class="app-message-card-frame"
      sandbox="allow-scripts"
      :srcdoc="srcdoc"
      :title="`${pluginId}/${viewId}`"
    />
    <!-- 轻量加载条（非全量覆盖层）：握手完成前显示 -->
    <div v-if="status === 'loading'" class="app-message-card-loading">
      <el-icon class="is-loading" :size="16"><Loading /></el-icon>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, type PropType } from 'vue';
import { Loading } from '@element-plus/icons-vue';
import type { PluginContext, PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { createBridgeHost, type BridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import { buildPluginHostSrcdoc, fetchPluginManifest } from '../../plugin-source';
import { createPluginBridgeDispatcher } from '../../plugin-bridge-dispatcher';
import { registerCard, routeCardAction, unregisterCard } from '../../plugin-card-actions';
import { themeMode } from '../../stores/theme';

/** 高度上下限（壳层封顶 400px，设计文档「UI 集成点」） */
const CARD_MIN_HEIGHT = 80;
const CARD_MAX_HEIGHT = 400;
const CARD_DEFAULT_HEIGHT = 180;

/** 壳层主题 → 插件 ctx theme（与 PluginIframeHost 同一判定） */
function resolveTheme(): 'light' | 'dark' {
  const dark =
    themeMode.value === 'dark' ||
    (themeMode.value === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  return dark ? 'dark' : 'light';
}

export default defineComponent({
  name: 'AppMessageCard',
  components: { Loading },
  props: {
    pluginId: { type: String, required: true },
    /** 卡片视图 id（应用消息 card.viewId，清单声明的 message-card 视图） */
    viewId: { type: String, required: true },
    /** 卡片视图数据（应用消息 card.data 透传） */
    cardData: { type: null as unknown as PropType<unknown>, default: undefined },
    /** 应用消息 id：cardId 派生材料（归属校验凭据） */
    messageId: { type: String, required: true },
    space: { type: Object as PropType<PluginSpaceContext>, required: true },
    /** 插件显示名（manifest；仅握手一致性提示用，缺省 pluginId） */
    pluginName: { type: String, default: '' }
  },
  emits: ['fallback'],
  setup(props, { emit }) {
    const iframeEl = ref<HTMLIFrameElement | null>(null);
    const status = ref<'loading' | 'ready' | 'failed'>('loading');
    const height = ref(CARD_DEFAULT_HEIGHT);

    // cardId 由壳层签发（插件自报一律忽略，见 onAction）；登记进归属映射
    const cardId = `${props.pluginId}:${props.messageId}`;
    const srcdoc = computed(() =>
      buildPluginHostSrcdoc(props.pluginId, {
        viewType: 'message-card',
        viewId: props.viewId,
        cardId,
        cardData: props.cardData
      })
    );

    let host: BridgeHost | null = null;
    // 代际令牌（与 PluginIframeHost 同模式）：卸载后进行中的 init 在下一个 await 即弃
    let generation = 0;

    const fail = (): void => {
      status.value = 'failed';
      emit('fallback');
    };

    const init = async (): Promise<void> => {
      const gen = ++generation;
      await nextTick();
      if (gen !== generation) {
        return;
      }
      const iframe = iframeEl.value;
      if (!iframe || !iframe.contentWindow) {
        fail();
        return;
      }
      const manifest = await fetchPluginManifest(props.pluginId);
      if (gen !== generation) {
        return;
      }
      const domain = `plugin:${props.pluginId}`;
      try {
        const handler = await createPluginBridgeDispatcher({
          pluginId: props.pluginId,
          viewId: props.viewId,
          domain,
          space: props.space,
          pluginName: props.pluginName || manifest?.name,
          supportedSpaces: manifest?.supportedSpaces,
          viewType: 'message-card'
        });
        if (gen !== generation) {
          return;
        }
        const ctx: PluginContext = {
          pluginId: props.pluginId,
          viewId: props.viewId,
          domain,
          space: props.space,
          theme: resolveTheme(),
          mount: { viewType: 'message-card', cardId, cardData: props.cardData }
        };
        host = createBridgeHost({
          iframe,
          pluginId: props.pluginId,
          viewId: props.viewId,
          // 沙箱 iframe 为 opaque origin（同 PluginIframeHost 口径）
          expectedOrigin: 'null',
          targetOrigin: '*',
          sdkVersion: manifest?.sdkVersion ?? '1',
          ctx,
          handler,
          onEvent: (event, payload) => {
            // 高度申请：壳层封顶 400px（payload 非法一律忽略）
            if (event !== 'card-resize') {
              return;
            }
            const requested = (payload as { height?: unknown } | undefined)?.height;
            if (typeof requested === 'number' && Number.isFinite(requested)) {
              height.value = Math.min(CARD_MAX_HEIGHT, Math.max(CARD_MIN_HEIGHT, Math.round(requested)));
            }
          },
          onAction: (_claimedCardId, actionId, data) => {
            // 归属校验在 plugin-card-actions（以桥绑定的 pluginId/cardId 为准，
            // 插件自报的 cardId 一律忽略）；主实例未运行时 action 丢弃（设计允许）
            routeCardAction(props.pluginId, cardId, actionId, data);
          }
        });
        await host.ready;
        if (gen !== generation) {
          return;
        }
        status.value = 'ready';
      } catch {
        if (gen !== generation) {
          return;
        }
        // 握手超时/版本不兼容/加载异常：降级为原生摘要（可达性不依赖插件代码）
        fail();
      }
    };

    onMounted(() => {
      registerCard(cardId, props.pluginId);
      void init();
    });
    onUnmounted(() => {
      generation += 1;
      unregisterCard(cardId);
      host?.destroy();
      host = null;
    });

    return { iframeEl, status, height, srcdoc };
  }
});
</script>

<style scoped>
.app-message-card {
  position: relative;
  width: 100%;
  max-width: 420px;
  border-radius: var(--spark-radius-xl);
  overflow: hidden;
  background: var(--spark-bg-card);
  box-shadow: var(--spark-shadow-card);
}

.app-message-card-frame {
  width: 100%;
  height: 100%;
  border: none;
  display: block;
}

.app-message-card-loading {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--el-text-color-secondary);
}
</style>
