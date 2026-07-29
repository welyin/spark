/**
 * 全局网络状态（模块级单例）——`p2p.info()` 的共享响应式快照。
 *
 * 顶栏 NetworkStatusBar、消息页（仅本地提示/发送失败）、邀请成员/添加朋友
 * 对话框都关心同一份 P2P 状态，收敛到这里统一轮询（30s）+ P2P 事件节流刷新，
 * 避免每个组件各自调接口。首个消费者 onMounted 时启动，应用生命周期内常驻。
 */
import { computed, onMounted, ref } from 'vue';
import { listenP2pEvents, type ElectronAPI, type P2pInfoDto } from '../api';

/** p2p.info 返回形状（与 api/types.ts 中 ElectronAPI['p2p']['info'] 一致） */
export type P2pInfo = P2pInfoDto;

/** 事件刷新最小间隔（连接/断开事件可能成对突发，避免连发 invoke）。 */
const EVENT_REFRESH_MIN_INTERVAL_MS = 2_000;
/** 轮询兜底周期。 */
const POLL_INTERVAL_MS = 30_000;

const info = ref<P2pInfo | null>(null);

/** 最近一次 p2p.info 快照（未拉取成功前为 null） */
export const p2pInfoSnapshot = info;

/** 仅本地判定：P2P 未启动才算仅本地。
 *  已启动但暂未连上任何节点不算——请求应照常发出，由内核排队/重试投递
 *  （送达失败走 failed + 重试路径），顶栏单独展示「暂未连接节点」。 */
export const isLocalOnly = computed<boolean>(() => !info.value?.started);

/** P2P 已启动但暂未连接到任何节点（弱提示，不阻断操作） */
export const isP2pIdle = computed<boolean>(() => !!info.value?.started && info.value.connectedPeers.length === 0);

/** 已连接节点数（未获取到时按 0 展示） */
export const connectedPeerCount = computed<number>(() => info.value?.connectedPeers.length ?? 0);

export async function refreshNetworkStatus(): Promise<void> {
  try {
    info.value = await window.electronAPI.p2p.info();
  } catch {
    // 状态读取失败不打扰用户，保留下一轮
  }
}

let started = false;
let lastEventRefreshAt = 0;

function start(): void {
  if (started) {
    return;
  }
  started = true;
  void refreshNetworkStatus();
  setInterval(() => void refreshNetworkStatus(), POLL_INTERVAL_MS);
  void (async () => {
    try {
      await listenP2pEvents(() => {
        const now = Date.now();
        if (now - lastEventRefreshAt < EVENT_REFRESH_MIN_INTERVAL_MS) {
          return;
        }
        lastEventRefreshAt = now;
        void refreshNetworkStatus();
      });
    } catch {
      // 非 Tauri 环境（单测/旧壳）无事件通道，仅靠轮询
    }
  })();
}

/** 组件内使用：挂载时确保轮询已启动，返回共享状态 */
export function useNetworkStatus() {
  onMounted(start);
  return { p2pInfoSnapshot, isLocalOnly, isP2pIdle, connectedPeerCount, refreshNetworkStatus };
}
