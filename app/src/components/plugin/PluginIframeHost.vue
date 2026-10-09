<template>
  <div class="plugin-iframe-host">
    <!-- 沙箱 iframe：allow-scripts，不给 allow-same-origin（opaque origin，
         localStorage/IndexedDB 与壳层天然隔离；桥握手 expectedOrigin 因此恒为 'null'）。
         宿主 HTML 由 srcdoc 内联生成（plugin/source.ts），bundle/css 经插件源加载。
         allow（Permissions-Policy）按 manifest.deviceCapabilities 最小化下发——
         iframe 须等 manifest 就绪后再创建：allow 只在 iframe 导航时生效，
         事后改属性不会回溯应用于已加载文档 -->
    <iframe
      v-if="manifestLoaded && status !== 'disabled' && status !== 'not-installed'"
      :key="reloadToken"
      ref="iframeEl"
      class="plugin-iframe-frame"
      sandbox="allow-scripts"
      :allow="iframeAllow"
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

    <!-- 已启用但未安装到本机（启用＝空间层逻辑状态，不要求代码在场；打开时就地安装，
         install-and-enable §一 形式化定义 + §五③ 按需获取） -->
    <div v-else-if="status === 'not-installed'" class="plugin-iframe-overlay">
      <p>「{{ notInstalledName }}」在本空间已启用，但还没安装到本机</p>
      <div class="plugin-iframe-overlay-actions">
        <el-button type="primary" :loading="installing" @click="installAndOpen">安装到本机并使用</el-button>
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
import type { PluginContext, PluginManifest, PluginSpaceContext } from '../../../../packages/plugin-sdk/src';
import { createBridgeHost, type BridgeHost } from '../../../../packages/plugin-sdk/src/bridge/host';
import { buildPluginHostSrcdoc, fetchPluginManifest } from '../../plugin/source';
import { createPluginBridgeDispatcher, setBridgeEventPump, type BridgeEventPump } from '../../plugin/bridge-dispatcher';
import { createPluginWatchdog, type PluginWatchdog } from '../../plugin/watchdog';
import { pluginSpaceKey, registerMainViewInstance, unregisterMainViewInstance } from '../../plugin/card-actions';
import { listenP2pEvents } from '../../api';
import {
  disablePluginInstance,
  enablePluginInstance,
  getDisabledPluginInstance,
  isPluginInstanceDisabled,
  pluginInstanceKey
} from '../../plugin/disabled';
import { themeMode } from '../../stores/theme';
import { installPluginItem } from '../apps/app-actions';
import { getMarketItem } from '../../stores/desktop/app-registry';
import type { PluginMarketItemDto } from '../../api/types';

type HostStatus = 'loading' | 'ready' | 'failed' | 'disabled' | 'not-installed';

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
    space: { type: Object as PropType<PluginSpaceContext>, required: true },
    /** 视图引导（可选）：注入 window.__sparkPluginView.cardData，如事务打开时传 affairId（3.4 深链） */
    viewBootstrap: { type: Object as PropType<{ cardData?: unknown }>, default: undefined }
  },
  emits: ['close', 'manifest'],
  setup(props, { emit }) {
    const iframeEl = ref<HTMLIFrameElement | null>(null);
    // 已停用实例首帧即覆盖层（iframe 不渲染，插件代码不加载）
    const instanceKey = pluginInstanceKey(props.pluginId, props.space);
    const status = ref<HostStatus>(isPluginInstanceDisabled(instanceKey) ? 'disabled' : 'loading');
    const disabledReason = ref(getDisabledPluginInstance(instanceKey)?.reason ?? '');
    const unresponsive = ref(false);
    const runtimeErrorCount = ref(0);
    const reloadToken = ref(0);
    /** manifest 就绪标记：iframe 延迟到 manifest 拉取后创建（allow 属性只在
     *  iframe 导航时生效；读取失败按 null manifest = 无设备能力放开降级） */
    const manifestLoaded = ref(false);
    const manifestRef = ref<PluginManifest | null>(null);
    /** Permissions-Policy allow：仅 manifest 声明的设备能力放行（最小化） */
    const iframeAllow = computed(() =>
      (manifestRef.value?.deviceCapabilities ?? []).includes('camera') ? 'camera' : ''
    );
    /** 「已启用未安装」打开时就地安装（install-and-enable §一/§五③） */
    const notInstalledName = ref('');
    const installing = ref(false);
    let marketItemRef: PluginMarketItemDto | null = null;
    const installAndOpen = async () => {
      const item = marketItemRef;
      if (!item || installing.value) {
        return;
      }
      installing.value = true;
      try {
        const ok = await installPluginItem(item);
        if (ok) {
          marketItemRef = { ...item, installed: true };
          await init();
        }
      } finally {
        installing.value = false;
      }
    };

    const srcdoc = computed(() =>
      buildPluginHostSrcdoc(props.pluginId, props.viewBootstrap ? { viewId: props.viewId, viewType: 'app', cardData: props.viewBootstrap.cardData } : undefined)
    );

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
      unlistenDataChanged?.();
      unlistenDataChanged = null;
      unlistenFeedReceived?.();
      unlistenFeedReceived = null;
      unlistenAffairChanged?.();
      unlistenAffairChanged = null;
      unlistenChatReceived?.();
      unlistenChatReceived = null;
      unlistenContactsSynced?.();
      unlistenContactsSynced = null;
      unlistenChatStatus?.();
      unlistenChatStatus = null;
      unlistenFriendRequests?.();
      unlistenFriendRequests = null;
      // 主视图实例登记清理（仅清自己：同插件新实例已接管时不误删）
      if (host) {
        unregisterMainViewInstance(props.pluginId, pluginSpaceKey(props.space), host);
      }
      host?.destroy();
      host = null;
      setBridgeEventPump(null);
    };

    /** 代际失效判定（过期时新建对象已由后到的 init/卸载经 destroyBridge 销毁） */
    const isStale = (gen: number): boolean => gen !== generation;

    // P6 远端数据合入通知：本插件实例的 p2p-event 订阅（init 建、销毁退）。
    // 事件名即 P2pEventDto kind（'PluginDataChanged'），与插件 SDK
    // spark.events.subscribe 的事件名同口径——插件按 payload.pluginId 自己
    // 过滤归属（payload 由内核侧按集合前缀聚合）。
    let unlistenDataChanged: (() => void) | null = null;
    // 社交投递入站推送（social-feed §8）：内核 FeedReceived → 桥 FeedReceived
    // 事件，按 topic 前缀（== pluginId）过滤后推给插件 sdk.feed.onReceive 订阅
    let unlistenFeedReceived: (() => void) | null = null;
    // 事务副本变更推送（sdk.affairs.onChange）：内核 AffairChanged → 桥事件。
    // 载荷为 affairId + 变更类别 + opHash/status（submitted）或 accepted/
    // drained 计数（replicated），均为哈希/计数无事务内容、无插件归属维度，
    // 经 affairs:read 授权门控后推给订阅了的实例（host.pushEvent 内部按
    // 订阅集合过滤；与 PluginDataChanged 同口径的轻量通知，插件收到后
    // 重读 readLog 收敛）
    let unlistenAffairChanged: (() => void) | null = null;
    // A18 IM 数据面（communication §4.1）：ChatReceived 按绑定 space 过滤 +
    // messages:read 授权门控后推给 sdk.messages.onNewMessage 订阅
    let unlistenChatReceived: (() => void) | null = null;
    // A18 通讯录数据面：ContactsSynced 经 contacts:read 授权门控后推给
    // sdk.contacts.onChanged 订阅（轻量通知，插件重读 overview 收敛）
    let unlistenContactsSynced: (() => void) | null = null;
    // A19 聊天/通讯录迁移事件面：ChatStatus 系与 FriendRequest 系监听
    let unlistenChatStatus: (() => void) | null = null;
    let unlistenFriendRequests: (() => void) | null = null;
    // A18 事件门控的授权清单数据源（与 bridge-dispatcher 同格：市场安装
    // 状态 grantedPermissions，渲染进程不可自报；读取失败按空清单 = 不推）
    const grantedPermissions = ref<Set<string>>(new Set());

    const init = async (): Promise<void> => {
      const gen = ++generation;
      destroyBridge();
      if (isPluginInstanceDisabled(instanceKey)) {
        disabledReason.value = getDisabledPluginInstance(instanceKey)?.reason ?? '';
        status.value = 'disabled';
        return;
      }
      // 启用≠安装（install-and-enable §一 形式化定义）：已启用未安装的应用可出现在
      // 桌面/启动器，打开时在此就地安装（代码按需获取，§五③）。
      // 判定读注册表缓存的市场条目（同步，不阻塞加载链路；缓存未至时按已装继续，
      // 宿主加载链自行报错——桌面挂载时 PcDesktop 已兜底 refreshAppRegistry）
      if (!props.pluginId.startsWith('spark:')) {
        const found = getMarketItem(props.pluginId);
        if (found && !found.installed) {
          notInstalledName.value = found.name;
          marketItemRef = found;
          status.value = 'not-installed';
          return;
        }
      }
      status.value = 'loading';
      unresponsive.value = false;
      runtimeErrorCount.value = 0;

      // manifest（best-effort）：supportedSpaces/显示名/设备能力声明；读取失败按无声明降级。
      // 必须先于 iframe 创建：Permissions-Policy allow 只在 iframe 导航时生效，
      // iframe 渲染由 manifestLoaded 门控
      const manifest = await fetchPluginManifest(props.pluginId);
      if (isStale(gen)) {
        return;
      }
      manifestRef.value = manifest;
      manifestLoaded.value = true;
      // 透传 manifest 给壳层（供顶栏按 chrome.hostTitleBar 决策隐藏/自接管）
      emit('manifest', manifest ?? null);

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
              viewType: 'app',
              // sdk.close()：插件请求关闭自身视图 → 通知壳层关闭当前插件 tab
              onClose: () => emit('close')
            }),
          onEvent: (event) => {
            if (event === 'runtime-error') {
              runtimeErrorCount.value += 1;
            }
          }
        });
        // 注入事件泵：sys.fetchStream 产生的 Tauri 事件经此泵转发为桥 event
        setBridgeEventPump({ pushEvent: (event, payload) => host!.pushEvent(event, payload) });

        await host.ready;
        if (isStale(gen)) {
          return;
        }
        status.value = 'ready';
        // 登记主视图实例：message-card 的按钮回调经 plugin/card-actions 按空间路由到本实例
        registerMainViewInstance(props.pluginId, pluginSpaceKey(props.space), host);
        // P6 数据变更转发：内核 PluginDataChanged → 桥 pushEvent（插件经
        // spark.events.subscribe('PluginDataChanged', fn) 或 spark.data.onChange
        // 接收；仅订阅时推送，host.pushEvent 内部按订阅集合过滤）
        void listenP2pEvents((event) => {
          if (event.kind !== 'PluginDataChanged') {
            return;
          }
          const data = event.data as { pluginId?: string } | undefined;
          if (data?.pluginId !== props.pluginId) {
            return;
          }
          host?.pushEvent('PluginDataChanged', event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenDataChanged = un;
          })
          .catch(() => {});
        // A18 事件门控授权清单：市场安装状态（读取失败按空清单 = 全部不推，
        // 与 dispatcher 最小授权同口径）
        try {
          const marketItems = await window.electronAPI.pluginMarket.list();
          grantedPermissions.value = new Set(
            marketItems.find((item) => item.id === props.pluginId)?.grantedPermissions ?? []
          );
        } catch {
          grantedPermissions.value = new Set();
        }
        if (isStale(gen)) {
          return;
        }
        // 社交投递入站推送（social-feed §8 + A18 §4.1 feed:read 门控）：
        // FeedReceived → 桥事件。topic 前缀 == 本插件 id 且已授权 feed:read
        // 才推送（topic 即插件归属，插件 sdk.feed.onReceive 内部再按订阅
        // topic 前缀做第二道收敛）。插件经 bridge client 的
        // events.subscribe('FeedReceived', fn) 接收（sdk.feed.onReceive 封装）。
        void listenP2pEvents((event) => {
          if (event.kind !== 'FeedReceived') {
            return;
          }
          if (!grantedPermissions.value.has('feed:read')) {
            return;
          }
          const data = event.data as { topic?: string } | undefined;
          const prefix = typeof data?.topic === 'string' ? data.topic.split(':')[0] ?? '' : '';
          if (prefix !== props.pluginId) {
            return;
          }
          host?.pushEvent('FeedReceived', event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenFeedReceived = un;
          })
          .catch(() => {});
        // A18 IM 数据面（communication §4.1）：ChatReceived → 桥事件。
        // 按绑定 space 过滤（payload.spaceKey 须一致）+ messages:read 授权
        // 门控（事件载荷含消息明文，与读面同权限位）。插件经
        // sdk.messages.onNewMessage（events.subscribe('ChatReceived')）接收。
        void listenP2pEvents((event) => {
          if (event.kind !== 'ChatReceived') {
            return;
          }
          if (!grantedPermissions.value.has('messages:read')) {
            return;
          }
          const data = event.data as { spaceKey?: string } | undefined;
          if (data?.spaceKey !== pluginSpaceKey(props.space)) {
            return;
          }
          host?.pushEvent('ChatReceived', event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenChatReceived = un;
          })
          .catch(() => {});
        // A18 通讯录数据面：ContactsSynced → 桥事件。contacts:read 授权门控
        // （轻量通知 {applied}，插件重读 overview 收敛，与 PluginDataChanged 同口径）
        void listenP2pEvents((event) => {
          if (event.kind !== 'ContactsSynced' && event.kind !== 'OrgSynced') {
            return;
          }
          if (!grantedPermissions.value.has('contacts:read')) {
            return;
          }
          host?.pushEvent(event.kind, event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenContactsSynced = un;
          })
          .catch(() => {});
        // A19 聊天应用迁移事件面（communication §4.2）：ChatStatus（已读/撤回/
        // 状态流转，space 过滤）/ ConversationsSynced / PeerConnected /
        // PeerDisconnected → 桥事件，messages:read 授权门控
        void listenP2pEvents((event) => {
          if (
            event.kind !== 'ChatStatus' &&
            event.kind !== 'ConversationsSynced' &&
            event.kind !== 'PeerConnected' &&
            event.kind !== 'PeerDisconnected'
          ) {
            return;
          }
          if (!grantedPermissions.value.has('messages:read')) {
            return;
          }
          if (event.kind === 'ChatStatus') {
            const data = event.data as { spaceKey?: string } | undefined;
            if (data?.spaceKey !== pluginSpaceKey(props.space)) {
              return;
            }
          }
          host?.pushEvent(event.kind, event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenChatStatus = un;
          })
          .catch(() => {});
        // A19 通讯录迁移事件面：FriendRequestReceived/Sent/Accepted +
        // FriendProfileUpdated → 桥事件，contacts:read 授权门控
        void listenP2pEvents((event) => {
          if (
            event.kind !== 'FriendRequestReceived' &&
            event.kind !== 'FriendRequestSent' &&
            event.kind !== 'FriendRequestAccepted' &&
            event.kind !== 'FriendProfileUpdated'
          ) {
            return;
          }
          if (!grantedPermissions.value.has('contacts:read')) {
            return;
          }
          host?.pushEvent(event.kind, event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenFriendRequests = un;
          })
          .catch(() => {});
        // 事务副本变更推送（sdk.affairs.onChange）：AffairChanged → 桥事件。
        // 无插件归属维度（affair 不属于任何插件），但按拍板口径「订阅归
        // affairs:read」（community-affairs §7.2 权限表）做授权门控——未授权
        // affairs:read 的插件连 affairId 变更通知也不应收（与读面同权限位）。
        // host.pushEvent 按订阅集合过滤，未订阅的实例零开销。
        void listenP2pEvents((event) => {
          if (event.kind !== 'AffairChanged') {
            return;
          }
          if (!grantedPermissions.value.has('affairs:read')) {
            return;
          }
          host?.pushEvent('AffairChanged', event.data);
        })
          .then((un) => {
            if (isStale(gen)) {
              un();
              return;
            }
            unlistenAffairChanged = un;
          })
          .catch(() => {});
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
      manifestLoaded,
      iframeAllow,
      srcdoc,
      reload,
      reenable,
      notInstalledName,
      installing,
      installAndOpen,
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
