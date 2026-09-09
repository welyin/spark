/**
 * 网络状态（插件 v1 桩）：壳层 useNetworkStatus 依赖内核 p2p 状态面，
 * 插件 v1 未接（后续可经 sdk 补）——恒为「非仅本地」（不显示离线提示条）。
 */
import { ref, type Ref } from 'vue';

/** 仅本地模式（无网络可达）：v1 恒 false */
export const isLocalOnly: Ref<boolean> = ref(false);

export function useNetworkStatus(): { isLocalOnly: Ref<boolean> } {
  return { isLocalOnly };
}
