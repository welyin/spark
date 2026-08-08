/**
 * 插件心跳熔断状态机（设计文档「熔断与治理」）。
 *
 * - 心跳 watchdog：ready 后每 5s 经桥 ping（连续 3 次超时标记「无响应」，
 *   恢复后自动解除标记）；宿主组件据此给「关闭/重新加载」覆盖层；
 * - 崩溃环自动停用：实例 60s 窗口内 ready 前错误 ≥3（握手超时/加载异常按一次计），
 *   或无响应累计 ≥3 次 → onAutoDisable，宿主组件自动停用当前 space 实例
 *   （持久化见 ./disabled.ts）；
 * - 计数按实例键驻留内存累计：iframe 重新挂载不清零（崩溃环语义）；
 *   手动重新启用时清零（./disabled.ts enablePluginInstance）。
 *
 * 纯逻辑模块（不依赖 vue / element-plus），vitest + fake timers 直测。
 */

/** 实例级累计计数（崩溃环判定依据） */
export type WatchdogCounters = {
  /** ready 前错误时间戳（ms；按 readyErrorWindowMs 滑动窗口裁剪） */
  readyErrorTimestamps: number[];
  /** 无响应累计次数（进入「无响应」状态一次计一次） */
  unresponsiveTrips: number;
};

/** 模块级计数表：按实例键累计，重新挂载共享 */
const countersByInstance = new Map<string, WatchdogCounters>();

export function getWatchdogCounters(instanceKey: string): WatchdogCounters {
  let entry = countersByInstance.get(instanceKey);
  if (!entry) {
    entry = { readyErrorTimestamps: [], unresponsiveTrips: 0 };
    countersByInstance.set(instanceKey, entry);
  }
  return entry;
}

/** 清零实例计数（手动重新启用时调用） */
export function clearWatchdogCounters(instanceKey: string): void {
  countersByInstance.delete(instanceKey);
}

export type PluginWatchdogOptions = {
  /** 实例键（pluginInstanceKey）：计数按此累计 */
  instanceKey: string;
  /** 心跳实现（host.ping 透传；超时 reject） */
  ping: (timeoutMs?: number) => Promise<void>;
  /** 心跳间隔（默认 5s） */
  heartbeatIntervalMs?: number;
  /** 单次 ping 超时（默认 5s） */
  pingTimeoutMs?: number;
  /** 连续超时次数阈值（默认 3，即 15s 无响应） */
  maxConsecutivePingFailures?: number;
  /** ready 前错误统计窗口（默认 60s） */
  readyErrorWindowMs?: number;
  /** 窗口内 ready 前错误阈值（默认 3） */
  maxReadyErrors?: number;
  /** 无响应累计阈值（默认 3） */
  maxUnresponsiveTrips?: number;
  /** 无响应状态变化回调（覆盖层显隐） */
  onUnresponsiveChange?: (unresponsive: boolean) => void;
  /** 崩溃环触发回调：宿主组件据此自动停用实例 */
  onAutoDisable?: (reason: 'ready-errors' | 'unresponsive') => void;
  /** 时钟注入（测试用） */
  now?: () => number;
};

export type PluginWatchdog = {
  /** ready 前错误计数（加载异常/握手超时按一次计）；窗口内超阈值触发自动停用 */
  recordReadyError: () => void;
  /** ready 后启动心跳循环（幂等） */
  startHeartbeat: () => void;
  /** 停止心跳并冻结状态机 */
  dispose: () => void;
  isUnresponsive: () => boolean;
};

export function createPluginWatchdog(options: PluginWatchdogOptions): PluginWatchdog {
  const intervalMs = options.heartbeatIntervalMs ?? 5_000;
  const pingTimeoutMs = options.pingTimeoutMs ?? 5_000;
  const maxPingFailures = options.maxConsecutivePingFailures ?? 3;
  const readyWindowMs = options.readyErrorWindowMs ?? 60_000;
  const maxReadyErrors = options.maxReadyErrors ?? 3;
  const maxTrips = options.maxUnresponsiveTrips ?? 3;
  const now = options.now ?? (() => Date.now());

  const counters = getWatchdogCounters(options.instanceKey);

  let timer: ReturnType<typeof setInterval> | null = null;
  let disposed = false;
  let pingInFlight = false;
  let consecutiveFailures = 0;
  let unresponsive = false;

  const autoDisable = (reason: 'ready-errors' | 'unresponsive'): void => {
    disposed = true;
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
    options.onAutoDisable?.(reason);
  };

  const beat = async (): Promise<void> => {
    if (disposed || pingInFlight) {
      return;
    }
    pingInFlight = true;
    try {
      await options.ping(pingTimeoutMs);
      consecutiveFailures = 0;
      if (unresponsive) {
        unresponsive = false;
        options.onUnresponsiveChange?.(false);
      }
    } catch {
      consecutiveFailures += 1;
      if (!unresponsive && consecutiveFailures >= maxPingFailures) {
        unresponsive = true;
        counters.unresponsiveTrips += 1;
        options.onUnresponsiveChange?.(true);
        if (counters.unresponsiveTrips >= maxTrips) {
          autoDisable('unresponsive');
          return;
        }
      }
    } finally {
      pingInFlight = false;
    }
  };

  return {
    recordReadyError() {
      if (disposed) {
        return;
      }
      const cutoff = now() - readyWindowMs;
      counters.readyErrorTimestamps = counters.readyErrorTimestamps.filter((t) => t >= cutoff);
      counters.readyErrorTimestamps.push(now());
      if (counters.readyErrorTimestamps.length >= maxReadyErrors) {
        autoDisable('ready-errors');
      }
    },
    startHeartbeat() {
      if (disposed || timer) {
        return;
      }
      timer = setInterval(() => void beat(), intervalMs);
    },
    dispose() {
      disposed = true;
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
    },
    isUnresponsive: () => unresponsive
  };
}
