/**
 * 插件实例熔断停用状态（设计文档「熔断与治理」崩溃环自动停用）。
 *
 * 持久化在 localStorage `spark:plugin-disabled`（实例键 → {reason, disabledAt}）：
 * 前端暂存口径，待内核插件实例状态接口落地后迁移（见 app/TODO.md）。
 * 实例级计数驻留内存（plugin-watchdog.ts），手动重新启用时一并清零。
 *
 * localStorage 读写一律 try/catch（隐私模式访问即抛，对齐 stores/theme.ts 口径）。
 */

import type { PluginSpaceContext } from '../../packages/plugin-sdk/src';
import { clearWatchdogCounters } from './plugin-watchdog';

const STORAGE_KEY = 'spark:plugin-disabled';

export type DisabledPluginInstance = {
  reason: string;
  disabledAt: number;
};

/** 实例键：插件在当前 space 的独立实例（同一插件跨 space 互不牵连） */
export function pluginInstanceKey(pluginId: string, space: PluginSpaceContext): string {
  return `${pluginId}@${space.type}:${space.id}`;
}

function readAll(): Record<string, DisabledPluginInstance> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, DisabledPluginInstance>;
      if (parsed && typeof parsed === 'object') {
        return parsed;
      }
    }
  } catch {
    // 数据损坏/不可读时按无停用处理
  }
  return {};
}

function writeAll(map: Record<string, DisabledPluginInstance>): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    // 持久化失败不影响当次停用（内存状态仍由宿主组件持有）
  }
}

export function getDisabledPluginInstance(instanceKey: string): DisabledPluginInstance | null {
  return readAll()[instanceKey] ?? null;
}

export function isPluginInstanceDisabled(instanceKey: string): boolean {
  return getDisabledPluginInstance(instanceKey) !== null;
}

/** 自动停用当前 space 实例（崩溃环触发；幂等） */
export function disablePluginInstance(instanceKey: string, reason: string): void {
  const all = readAll();
  all[instanceKey] = { reason, disabledAt: Date.now() };
  writeAll(all);
}

/** 手动重新启用：移除停用标记并清零熔断计数 */
export function enablePluginInstance(instanceKey: string): void {
  const all = readAll();
  delete all[instanceKey];
  writeAll(all);
  clearWatchdogCounters(instanceKey);
}
