<template>
  <div class="plugin-iframe-host">
    <!-- 沙箱 iframe：allow-scripts，不给 allow-same-origin（opaque origin，
         localStorage/IndexedDB 与壳层天然隔离；桥握手 expectedOrigin 因此恒为 'null'）。
         宿主 HTML 由 srcdoc 内联生成（plugin/source.ts），bundle/css 经插件源加载 -->
    <iframe
      v-if="status !== 'disabled'"
      :key="reloadToken"
      ref="iframeEl"
      class="plugin-iframe-frame"
      sandbox="allow-scripts"
      :srcdoc="srcdoc"
      :title="`${pluginId}/${viewId}`"
    />

    <!-- 加载中 -->
    <div v-if="status === 'loading'" class="plugin-iframe-overlay">
      <el-icon class="is-loading" :size="28"><Loading /></el-icon>
      <p>插件加载中…</p>
    </div>

    <!-- 加载失败（握手超时/版本不兼容/加载异常） -->
    <div v-else-if="status === 'failed'" class="plugin-iframe-overlay">
      <p>插件加载失败</p>
      <div class="plugin-iframe-overlay-actions">
        <el-button type="primary" @click="reload">重新加载</el-button>
        <el-button @click="emit('close')">关闭</el-button>
      </div>
    </div>

    <!-- 崩溃环自动停用 -->
    <div v-else-if="status === 'disabled'" class="plugin-iframe-overlay">
      <p>该插件在当前空间因多次异常已自动停用</p>
      <p v-if="disabledReason" class="plugin-iframe-overlay-reason">原因：{{ disabledReasonText }}</p>
      <div class="plugin-iframe-overlay-actions">
        <el-button type="primary" @click="reenable">重新启用</el-button>
        <el-button @click="emit('close')">关闭</el-button>
      </div>
    </div>

    <!-- 心跳无响应（覆盖在已加载内容之上） -->
    <div v-else-if="unresponsive" class="plugin-iframe-overlay">
      <p>插件无响应</p>
      <p v-if="runtimeErrorCount > 0" class="plugin-iframe-overlay-reason">
        已上报 {{ runtimeErrorCount }} 个运行时错误
      </p>
      <div class="plugin-iframe-overlay-actions">
        <el-button type="primary" @click="reload">重新加载</el-button>
        <el-button @click="emit('close')">关闭</el-button>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, type PropType } from 'vue';
import { Loading } from '@element-plus/icons-vue';
import type { PluginContext, PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { createBridgeHost, type BridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import { buildPluginHostSrcdoc, fetchPluginManifest } from '../../plugin/source';
import { createPluginBridgeDispatcher } from '../../plugin/bridge-dispatcher';
import { createPluginWatchdog, type PluginWatchdog } from '../../plugin/watchdog';
import { pluginSpaceKey, registerMainViewInstance, unregisterMainViewInstance } from '../../plugin/card-actions';
import {
  disablePluginInstance,
  enablePluginInstance,
  getDisabledPluginInstance,
  isPluginInstanceDisabled,
  pluginInstanceKey
} from '../../plugin/disabled';
import { themeMode } from '../../stores/theme';

type HostStatus = 'loading' | 'ready' | 'failed' | 'disabled';

/** 壳层主题 → 插件 ctx theme（与 stores/theme.ts apply 同一判定） */
function resolveTheme(): 'light' | 'dark' {
  const dark =
    themeMode.value === 'dark' ||
    (themeMode.value === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  return dark ? 'dark' : 'light';
}

export default defineComponent({
  name: 'PluginIframeHost',
  components: { Loading },
  props: {
    pluginId: { type: String, required: true },
    viewId: { type: String, required: true },
    space: { type: Object as PropType<PluginSpaceContext>, required: true }
  },
  emits: ['close'],
  setup(props, { emit }) {
    const iframeEl = ref<HTMLIFrameElement | null>(null);
    // 已停用实例首帧即覆盖层（iframe 不渲染，插件代码不加载）
    const instanceKey = pluginInstanceKey(props.pluginId, props.space);
    const status = ref<HostStatus>(isPluginInstanceDisabled(instanceKey) ? 'disabled' : 'loading');
    const disabledReason = ref(getDisabledPluginInstance(instanceKey)?.reason ?? '');
    const unresponsive = ref(false);
    const runtimeErrorCount = ref(0);
    const reloadToken = ref(0);

    const srcdoc = computed(() => buildPluginHostSrcdoc(props.pluginId));

    const disabledReasonText = computed(() =>
      disabledReason.value === 'ready-errors'
        ? '启动阶段连续异常'
        : disabledReason.value === 'unresponsive'
          ? '反复无响应'
          : disabledReason.value
    );

    let host: BridgeHost | null = null;
    let watchdog: PluginWatchdog | null = null;
    // 代际令牌（init 竞态防护）：每次 init/卸载递增，await 后校验，过期即弃——
    // 保证任意时序下只有一个活 host，不会出现双 handler
    let generation = 0;

    const destroyBridge = (): void => {
      watchdog?.dispose();
      watchdog = null;
      // 主视图实例登记清理（仅清自己：同插件新实例已接管时不误删）
      if (host) {
        unregisterMainViewInstance(props.pluginId, pluginSpaceKey(props.space), host);
      }
      host?.destroy();
      host = null;
    };

    /** 代际失效判定（过期时新建对象已由后到的 init/卸载经 destroyBridge 销毁） */
    const isStale = (gen: number): boolean => gen !== generation;

    const init = async (): Promise<void> => {
      const gen = ++generation;
      destroyBridge();
      if (isPluginInstanceDisabled(instanceKey)) {
        disabledReason.value = getDisabledPluginInstance(instanceKey)?.reason ?? '';
        status.value = 'disabled';
        return;
      }
      status.value = 'loading';
      unresponsive.value = false;
      runtimeErrorCount.value = 0;

      // reload 后 iframe 经 :key 重建，等 DOM 更新再取 contentWindow
      await nextTick();
      if (isStale(gen)) {
        return;
      }
      const iframe = iframeEl.value;
      if (!iframe || !iframe.contentWindow) {
        status.value = 'failed';
        return;
      }

      // manifest（best-effort）：supportedSpaces 与显示名；读取失败按无声明降级
      const manifest = await fetchPluginManifest(props.pluginId);
      if (isStale(gen)) {
        return;
      }
      const domain = `plugin:${props.pluginId}`;

      watchdog = createPluginWatchdog({
        instanceKey,
        ping: (timeoutMs) => (host ? host.ping(timeoutMs) : Promise.reject(new Error('bridge not ready'))),
        onUnresponsiveChange: (value) => {
          unresponsive.value = value;
        },
        onAutoDisable: (reason) => {
          disablePluginInstance(instanceKey, reason);
          disabledReason.value = reason;
          status.value = 'disabled';
          destroyBridge();
        }
      });

      try {
        const ctx: PluginContext = {
          pluginId: props.pluginId,
          viewId: props.viewId,
          domain,
          space: props.space,
          theme: resolveTheme(),
          mount: { viewType: 'app' }
        };

        // 关键时序：createBridgeHost 必须同步执行——它内部注册 message 监听接收
        // 插件 hello。若先 await createPluginBridgeDispatcher（内含 Tauri invoke
        // 读授权清单），监听注册被推迟，hello 发出时无人接收 → 握手超时。
        // 故 handler 传懒解析函数，首个 call 到来时（握手已完成）才解析 dispatcher。
        host = createBridgeHost({
          iframe,
          pluginId: props.pluginId,
          viewId: props.viewId,
          // 沙箱 iframe 为 opaque origin：生产/dev 一致为 'null'（source 校验 +
          // hello 身份核对补偿 origin 不可区分性，见 bridge/host.ts）；
          // opaque origin 下 postMessage targetOrigin 只能为 '*'
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
              viewType: 'app'
            }),
          onEvent: (event) => {
            if (event === 'runtime-error') {
              runtimeErrorCount.value += 1;
            }
          }
        });

        await host.ready;
        if (isStale(gen)) {
          return;
        }
        status.value = 'ready';
        // 登记主视图实例：message-card 的按钮回调经 plugin/card-actions 按空间路由到本实例
        registerMainViewInstance(props.pluginId, pluginSpaceKey(props.space), host);
        watchdog?.startHeartbeat();
      } catch {
        // 过期代的失败不计数不落地（新一代已接管，避免误记 ready 前错误）
        if (isStale(gen)) {
          return;
        }
        // 握手超时/版本不兼容按一次 ready 前错误计（设计文档「熔断与治理」）
        if (watchdog) {
          watchdog.recordReadyError();
        }
        if (!isPluginInstanceDisabled(instanceKey)) {
          status.value = 'failed';
        }
      }
    };

    /** 重新加载：重建 iframe（:key 变化）与桥实例 */
    const reload = (): void => {
      reloadToken.value += 1;
      void init();
    };

    /** 手动重新启用：清零计数后重新加载 */
    const reenable = (): void => {
      enablePluginInstance(instanceKey);
      disabledReason.value = '';
      reload();
    };

    onMounted(() => void init());
    // 卸载同样使代际失效：进行中的 init 在下一个 await 后即弃，不再落状态
    onUnmounted(() => {
      generation += 1;
      destroyBridge();
    });

    return {
      iframeEl,
      status,
      disabledReason,
      disabledReasonText,
      unresponsive,
      runtimeErrorCount,
      reloadToken,
      srcdoc,
      reload,
      reenable,
      emit
    };
  }
});
</script>

<style scoped>
.plugin-iframe-host {
  position: relative;
  width: 100%;
  height: 100%;
  min-height: 480px;
}

.plugin-iframe-frame {
  width: 100%;
  height: 100%;
  min-height: 480px;
  border: none;
  display: block;
}

.plugin-iframe-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  background: var(--el-bg-color);
  color: var(--el-text-color-regular);
  z-index: 1;
}

.plugin-iframe-overlay-reason {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}

.plugin-iframe-overlay-actions {
  display: flex;
  gap: 8px;
}
</style>
