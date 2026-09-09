<!-- 默认内置应用挂载区（communication §4.2 灰度，A19）：按灰度开关渲染
     旧内置 UI（#legacy 插槽）或默认内置插件版（PluginIframeHost 沙箱链路，
     与插件 tab 同一承载）。插件加载失败点「关闭」经 fallback 回退旧 UI。 -->
<template>
  <slot v-if="impl === 'legacy' || !def" name="legacy" />
  <div v-else class="builtin-app-host">
    <PluginIframeHost
      :key="`builtin|${def.pluginId}|${space.id}`"
      :plugin-id="def.pluginId"
      :view-id="def.viewId"
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
    /** 主 tab id（'messages' / 'contacts'），经注册表查默认内置插件 */
    tabId: { type: String, required: true },
    /** 插件运行 space 上下文（切换经 :key 重建实例） */
    space: { type: Object as PropType<PluginSpaceContext>, required: true }
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
