<!-- 默认内置应用挂载区（communication §4.2 灰度，A19）：按灰度开关渲染
     旧内置 UI（#legacy 插槽）或默认内置插件版（PluginIframeHost 沙箱链路，
     与插件 tab 同一承载）。插件加载失败点「关闭」经 fallback 回退旧 UI。
     initialView（A34）：直达子视图意图透传插件版（与 legacy 的 initial-view
     同口径，如 Dock「应用市场」直达 market 页）；空串/未传用注册表入口视图。 -->
<template>
  <slot v-if="impl === 'legacy' || !def" name="legacy" />
  <div v-else class="builtin-app-host">
    <PluginIframeHost
      :key="`builtin|${def.pluginId}|${space.id}`"
      :plugin-id="def.pluginId"
      :view-id="initialView || def.viewId"
      :space="space"
      @close="emit('fallback')"
      @manifest="() => {}"
    />
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import type { PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import PluginIframeHost from './PluginIframeHost.vue';
import { builtinImpl, builtinPluginFor } from '../../stores/builtin-apps';

export default defineComponent({
  name: 'BuiltinAppHost',
  components: { PluginIframeHost },
  props: {
    /** 主 tab id（'messages' / 'apps'），经注册表查默认内置插件 */
    tabId: { type: String, required: true },
    /** 插件运行 space 上下文（切换经 :key 重建实例） */
    space: { type: Object as PropType<PluginSpaceContext>, required: true },
    /** 直达子视图 id（须为插件 manifest 已声明视图；空串 = 注册表入口视图） */
    initialView: { type: String, default: '' }
  },
  emits: ['fallback'],
  setup(props, { emit }) {
    const def = computed(() => builtinPluginFor(props.tabId));
    // 渲染期间读取响应式开关：设置页切换后本组件自动重渲染
    const impl = computed(() => builtinImpl(props.tabId));
    return { def, impl, emit };
  }
});
</script>

<style scoped>
.builtin-app-host {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
}
</style>
