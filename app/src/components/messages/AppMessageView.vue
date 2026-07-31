<!-- 单条应用消息（§20 服务号模型）：card 存在且插件已安装启用 → message-card
     iframe 富渲染（AppMessageCard）；card 不存在或插件未安装/未启用/卡片加载失败
     → 原生摘要降级（可达性不依赖插件代码）。未安装时提供「安装插件查看完整内容」
     入口（跳应用市场详情）。 -->
<template>
  <div class="app-msg">
    <div class="app-msg-head">
      <span class="app-msg-name">{{ pluginDisplayName }}</span>
      <span class="app-msg-tag">应用消息</span>
    </div>

    <!-- 富渲染：卡片加载失败经 fallback 事件降级为摘要 -->
    <AppMessageCard
      v-if="renderCard"
      :plugin-id="message.pluginId"
      :view-id="message.card!.viewId"
      :card-data="message.card!.data"
      :message-id="message.id"
      :space="space"
      :plugin-name="pluginDisplayName"
      @fallback="cardFailed = true"
    />

    <!-- 原生摘要降级 -->
    <div v-else class="app-msg-summary">
      <p class="app-msg-text">{{ message.summary }}</p>
      <button v-if="showInstallLink" type="button" class="app-msg-install" @click="$emit('open-market', message.pluginId)">
        安装插件查看完整内容
      </button>
      <p v-else-if="showEnableHint" class="app-msg-hint">启用插件查看完整内容</p>
      <p v-else-if="cardFailed" class="app-msg-hint">卡片加载失败，已显示摘要</p>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import type { AppMessageDto } from '../../api/types';
import { appConversationName, isAppInstalled, isAppUsable } from '../../stores/app-conversations';
import AppMessageCard from './AppMessageCard.vue';

export default defineComponent({
  name: 'AppMessageView',
  components: { AppMessageCard },
  props: {
    message: { type: Object as PropType<AppMessageDto>, required: true },
    /** 会话空间 key（'personal' / 'org:<orgId>'）：启用状态判定用 */
    spaceKey: { type: String, required: true },
    /** 卡片 iframe 的运行上下文（与 PluginIframeHost 同一来源） */
    space: { type: Object as PropType<PluginSpaceContext>, required: true }
  },
  emits: ['open-market'],
  setup(props) {
    /** 卡片 iframe 加载失败：降级为摘要（一次性，不重试——重进会话自然重试） */
    const cardFailed = ref(false);

    const pluginDisplayName = computed(() => appConversationName(props.message.pluginId));

    // 富渲染条件：携带 card + 插件已安装启用 + 卡片未加载失败
    const renderCard = computed(
      () => !!props.message.card && !cardFailed.value && isAppUsable(props.message.pluginId, props.spaceKey)
    );

    // 降级文案：未安装 → 安装入口；已安装未启用 → 启用提示
    const showInstallLink = computed(() => !!props.message.card && !isAppInstalled(props.message.pluginId));
    const showEnableHint = computed(
      () => !!props.message.card && isAppInstalled(props.message.pluginId) && !cardFailed.value
    );

    return { cardFailed, pluginDisplayName, renderCard, showInstallLink, showEnableHint };
  }
});
</script>
