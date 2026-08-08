<template>
  <!-- 后台常驻视图宿主：隐藏 iframe（无 UI），随插件启用启动、随应用存活。
       用于承载插件的常驻任务（如 bot 消息监听），不依赖任何可见视图。
       与 PluginIframeHost 的区别：无五态覆盖层/无崩溃环 UI/无 close 事件——
       后台视图对用户不可见，失败静默重试，由 watchdog 负责熔断。 -->
  <iframe
    v-show="false"
    ref="iframeEl"
    class="plugin-background-frame"
    sandbox="allow-scripts"
    :srcdoc="srcdoc"
    :title="`${pluginId}/background`"
    aria-hidden="true"
  />
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, type PropType } from 'vue';
import type { PluginContext, PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { createBridgeHost, type BridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import { buildPluginHostSrcdoc, fetchPluginManifest } from '../../plugin/source';
import { createPluginBridgeDispatcher } from '../../plugin/bridge-dispatcher';
import { pluginSpaceKey } from '../../plugin/card-actions';
import { markBotOnline, markBotOffline, registerBackgroundHost, unregisterBackgroundHost } from '../../plugin/bot-presence';
import { themeMode } from '../../stores/theme';

/** 壳层主题 → 插件 ctx theme（与 PluginIframeHost 同一判定） */
function resolveTheme(): 'light' | 'dark' {
  const dark =
    themeMode.value === 'dark' ||
    (themeMode.value === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  return dark ? 'dark' : 'light';
}

export default defineComponent({
  name: 'PluginBackgroundHost',
  props: {
    pluginId: { type: String, required: true },
    /** 后台视图 id（manifest 中 type:'background' 的视图 id） */
    viewId: { type: String, required: true },
    space: { type: Object as PropType<PluginSpaceContext>, required: true }
  },
  setup(props) {
    const iframeEl = ref<HTMLIFrameElement | null>(null);
    // 必须传 mount：注入 window.__sparkPluginView（viewId/viewType），插件入口据此
    // 以正确 viewId 握手（background 视图缺失会回退 'chat' → identity mismatch）
    const srcdoc = computed(() =>
      buildPluginHostSrcdoc(props.pluginId, { viewId: props.viewId, viewType: 'background' })
    );

    let host: BridgeHost | null = null;
    let cancelled = false;

    async function init(): Promise<void> {
      const iframe = iframeEl.value;
      if (!iframe) return;
      let manifest;
      try {
        manifest = await fetchPluginManifest(props.pluginId);
      } catch {
        return; // 清单不可读：静默放弃（后台视图无 UI，不打扰用户）
      }
      if (cancelled) return;

      const domain = `plugin:${props.pluginId}`;
      const ctx: PluginContext = {
        pluginId: props.pluginId,
        viewId: props.viewId,
        domain,
        space: props.space,
        theme: resolveTheme(),
        mount: { viewType: 'background' }
      };

      try {
        console.log(`[plugin-background] 初始化 ${props.pluginId}/${props.viewId}`);
        // 与 PluginIframeHost 同口径：createBridgeHost 同步注册 message 监听，
        // handler 懒解析（dispatcher 的 Tauri invoke 不阻塞监听注册，防握手超时）
        host = createBridgeHost({
          iframe,
          pluginId: props.pluginId,
          viewId: props.viewId,
          expectedOrigin: 'null',
          targetOrigin: '*',
          sdkVersion: manifest?.sdkVersion ?? '1',
          ctx,
          handler: () =>
            createPluginBridgeDispatcher({
              pluginId: props.pluginId,
              viewId: props.viewId,
              domain,
              space: props.space,
              pluginName: manifest?.name,
              supportedSpaces: manifest?.supportedSpaces,
              viewType: 'background'
            })
        });

        await host.ready;
        if (cancelled) return;
        markBotOnline(props.pluginId);
        registerBackgroundHost(props.pluginId, host);
        console.log(`[plugin-background] ${props.pluginId} 握手成功，常驻任务已启动`);
        // 后台视图就绪：插件的常驻任务（消息监听等）此刻已在运行
      } catch (err) {
        // 握手失败：静默记录，不打扰用户（后台视图无 UI）
        console.warn(`[plugin-background] ${props.pluginId} 握手失败:`, err);
      }
    }

    onMounted(async () => {
      await nextTick();
      await init();
    });

    onUnmounted(() => {
      cancelled = true;
      markBotOffline(props.pluginId);
      unregisterBackgroundHost(props.pluginId);
      host?.destroy();
      host = null;
    });

    return { iframeEl, srcdoc };
  }
});
</script>

<style scoped>
.plugin-background-frame {
  width: 0;
  height: 0;
  border: none;
  position: absolute;
  pointer-events: none;
}
</style>
