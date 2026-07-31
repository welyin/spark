/**
 * 应用会话壳层辅助（p2p-messages.md §20 / plugin_system.md「应用会话（服务号模型）」）。
 *
 * - 显示名解析：应用会话内核标题缺省为 pluginId，壳层按插件清单名称展示
 *   （pluginMarket.list 聚合，含未安装条目；内置 'system' 恒为「系统通知」）；
 * - 安装/启用状态：卡片是否走 iframe 富渲染的判定数据源（未安装/未启用 →
 *   原生摘要降级）；组织空间启用状态读 apps-store 的 localStorage 口径；
 * - 屏蔽状态：按空间 localStorage 持久化（spark:app-conv-blocked），被屏蔽会话
 *   未读角标与未读聚合一律抑制，列表仍可见（可就地取消屏蔽）。
 *
 * 非 Tauri 环境（纯浏览器/单测）无市场数据：名称回退 pluginId，一律按未安装处理。
 */
import { ref } from 'vue';
import { isTauri, type ElectronAPI, type PluginMarketItemDto } from '../api';

/** 内置系统应用会话 pluginId（系统通知写入入口，app-messages.ts） */
export const SYSTEM_APP_PLUGIN_ID = 'system';
/** 内置系统应用会话显示名 */
export const SYSTEM_APP_NAME = '系统通知';

// ------------------------------------------------------------------
// 市场条目（名称与安装/启用状态的数据源，懒加载一次，失败允许下次重试）
// ------------------------------------------------------------------

const marketItems = ref<PluginMarketItemDto[]>([]);
let marketLoaded = false;

function ensureMarketLoaded(): void {
  if (marketLoaded || !isTauri()) {
    return;
  }
  marketLoaded = true;
  const api = (window as unknown as { electronAPI?: ElectronAPI }).electronAPI;
  void api?.pluginMarket
    .list()
    .then((items) => {
      marketItems.value = items;
    })
    .catch(() => {
      marketLoaded = false; // 读取失败：下次访问重试
    });
}

/** 应用会话显示名：插件清单名称（缺省 pluginId；'system' 恒为「系统通知」） */
export function appConversationName(pluginId: string, fallback?: string): string {
  if (pluginId === SYSTEM_APP_PLUGIN_ID) {
    return SYSTEM_APP_NAME;
  }
  ensureMarketLoaded();
  return marketItems.value.find((item) => item.id === pluginId)?.name ?? fallback ?? pluginId;
}

/** 插件是否已安装（卡片富渲染前置条件之一；非 Tauri 恒 false → 摘要降级） */
export function isAppInstalled(pluginId: string): boolean {
  if (pluginId === SYSTEM_APP_PLUGIN_ID) {
    return false; // 内置系统会话无插件代码，恒走摘要渲染
  }
  ensureMarketLoaded();
  return marketItems.value.find((item) => item.id === pluginId)?.installed ?? false;
}

/** 组织空间启用状态存储键（与 apps-store useOrgEnabled 同一 localStorage 口径） */
function orgEnabledKey(spaceKey: string): string {
  return `spark:apps-org-enabled:${spaceKey}`;
}

/**
 * 插件是否「已安装且启用」（卡片 iframe 富渲染判定）：
 * 个人空间取市场条目 enabled；组织空间取管理员启用状态（localStorage 口径，
 * 与 apps-store useOrgEnabled 同源，best-effort 读取）。
 */
export function isAppUsable(pluginId: string, spaceKey: string): boolean {
  ensureMarketLoaded();
  const item = marketItems.value.find((entry) => entry.id === pluginId);
  if (!item?.installed) {
    return false;
  }
  if (spaceKey === 'personal') {
    return item.enabled;
  }
  try {
    const raw = localStorage.getItem(orgEnabledKey(spaceKey));
    const map = raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
    return map[pluginId] ?? false;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------
// 屏蔽（本地持久化；被屏蔽会话抑制未读角标与聚合，列表仍可见可取消）
// ------------------------------------------------------------------

const BLOCKED_STORAGE_KEY = 'spark:app-conv-blocked';

function loadBlocked(): Record<string, string[]> {
  try {
    const raw = localStorage.getItem(BLOCKED_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      const result: Record<string, string[]> = {};
      for (const [key, value] of Object.entries(parsed)) {
        if (Array.isArray(value)) {
          result[key] = value.filter((item): item is string => typeof item === 'string');
        }
      }
      return result;
    }
  } catch {
    // 本地存储不可读/数据损坏时按无屏蔽处理
  }
  return {};
}

/** Record<spaceKey, pluginId[]> */
const blocked = ref<Record<string, string[]>>(loadBlocked());

export function isAppConversationBlocked(spaceKey: string, pluginId: string): boolean {
  return (blocked.value[spaceKey] ?? []).includes(pluginId);
}

export function toggleAppConversationBlocked(spaceKey: string, pluginId: string): void {
  const list = blocked.value[spaceKey] ?? [];
  const next = list.includes(pluginId) ? list.filter((id) => id !== pluginId) : [...list, pluginId];
  blocked.value = { ...blocked.value, [spaceKey]: next };
  try {
    localStorage.setItem(BLOCKED_STORAGE_KEY, JSON.stringify(blocked.value));
  } catch {
    // 持久化失败不阻断交互（重启后丢失）
  }
}
