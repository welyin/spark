/**
 * 应用页共享常量、展示助手与本地 mock 存储（ui-apps-market §2/§3）。
 *
 * 真实数据一律来自 window.electronAPI.pluginMarket；内核尚未提供的
 * 归属/状态字段（分组归属、组织启用状态等）在此用 localStorage 模拟，
 * 每处都有 TODO(mock) 标注，待内核接口就绪后替换。
 */
import { computed, ref, watch, type Ref } from 'vue';
import type { PluginMarketItemDto } from '../../api/types';
import { hashGradient } from '../../utils/palette';

/** 应用图标配色：按插件 id 哈希取渐变（与 UserAvatar 自动头像同一套色板） */
export function appIconBackground(item: PluginMarketItemDto): string {
  return hashGradient(item.id || item.name);
}

/** 应用列表默认分组（ui-apps-market §2.2，暂定分类）。 */
export const LIST_GROUPS = ['常用', '办公', '社交', '工具', '其他'] as const;

/** 应用市场分类（ui-apps-market §3.3，不含「全部」；与 category 枚举的映射见 marketCategoryOf）。 */
export type MarketCategory = '基础' | '社交' | '工具' | 'AI 助手' | '游戏' | '其他';
export const MARKET_CATEGORIES: MarketCategory[] = ['基础', '社交', '工具', 'AI 助手', '游戏', '其他'];

/** 权限标识的中文展示名（沿用旧版插件市场页）。 */
export const PERMISSION_LABELS: Record<string, string> = {
  'storage:read': '读取本域数据',
  'storage:write': '写入本域数据',
  'org:read': '读取组织信息',
  'org:sync': '同步组织数据',
  'network:broadcast': '网络广播',
  'proof:verify': '存证核验',
  'identity:sign': '域身份签名',
  'message:app': '发送应用消息'
};

export function permissionLabel(permission: string): string {
  return PERMISSION_LABELS[permission] ?? permission;
}

export function marketCategoryOf(item: PluginMarketItemDto): MarketCategory {
  // 语义化分类直接映射（市场数据带 category 枚举后不再需要关键字匹配）
  switch (item.category) {
    case 'foundation':
      return '基础';
    case 'ai-assistant':
      return 'AI 助手';
    case 'social':
      return '社交';
    case 'tool':
      return '工具';
    case 'game':
      return '游戏';
    default:
      return '其他';
  }
}

/** 市场搜索：按名称、简介、开发者（域名）过滤（ui-apps-market §3.2）。 */
export function marketItemMatches(item: PluginMarketItemDto, keyword: string): boolean {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return true;
  }
  return [item.name, item.description, item.domain].some((field) =>
    field.toLowerCase().includes(kw)
  );
}

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      return JSON.parse(raw) as T;
    }
  } catch {
    // 本地存储不可读/数据损坏时按默认值处理
  }
  return fallback;
}

function saveJson(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // 持久化失败不阻断交互（重启后丢失）
  }
}

type GroupsPayload = {
  assignments: Record<string, string>;
  /** 全量分组列表（含内置与自定义，可增删改） */
  groups?: string[];
  /** 老版本字段：仅自定义分组（加载时迁移进 groups） */
  customGroups?: string[];
  /** 组内排序：插件 id → 序号（小的在前，缺省按原始顺序排在有排序值的后面） */
  orders?: Record<string, number>;
};

// TODO(mock): 应用分组归属内核没有接口，先按空间存 localStorage；待内核提供分组模型后替换
export function useAppGroups(spaceKey: Ref<string>) {
  const assignments = ref<Record<string, string>>({});
  const groups = ref<string[]>([...LIST_GROUPS]);
  const orders = ref<Record<string, number>>({});
  const storageKey = computed(() => `spark:apps-groups:${spaceKey.value}`);

  const load = () => {
    const payload = loadJson<GroupsPayload>(storageKey.value, { assignments: {}, groups: [...LIST_GROUPS] });
    assignments.value = payload.assignments ?? {};
    // 老数据迁移：内置常量 + customGroups；新数据直接读 groups
    groups.value = payload.groups ?? [...LIST_GROUPS, ...(payload.customGroups ?? [])];
    if (groups.value.length === 0) {
      groups.value = [...LIST_GROUPS];
    }
    orders.value = payload.orders ?? {};
  };
  watch(spaceKey, load, { immediate: true });

  const save = () => {
    saveJson(storageKey.value, { assignments: assignments.value, groups: groups.value, orders: orders.value });
  };

  const allGroups = computed<string[]>(() => groups.value);

  /** 未指派/分组已删除时的回退分组：优先「其他」，否则第一个分组 */
  const fallbackGroup = (): string => (groups.value.includes('其他') ? '其他' : groups.value[0]);

  const groupOf = (pluginId: string): string => {
    const group = assignments.value[pluginId];
    return group && groups.value.includes(group) ? group : fallbackGroup();
  };

  const moveToGroup = (pluginId: string, group: string) => {
    assignments.value = { ...assignments.value, [pluginId]: group };
    save();
  };

  /** 组内排序值（缺省 undefined = 排在有排序值的卡片之后，按原始顺序） */
  const orderOf = (pluginId: string): number | undefined => orders.value[pluginId];

  /** 持久化某分组内的完整顺序（0..n 归一化写入；其它分组的排序值不受影响） */
  const persistOrder = (orderedIds: string[]) => {
    const next = { ...orders.value };
    orderedIds.forEach((id, index) => {
      next[id] = index;
    });
    orders.value = next;
    save();
  };

  /** 新建分组；重名或空名返回 false。 */
  const createGroup = (name: string): boolean => {
    const trimmed = name.trim();
    if (!trimmed || groups.value.includes(trimmed)) {
      return false;
    }
    groups.value = [...groups.value, trimmed];
    save();
    return true;
  };

  /** 重命名分组（含内置分组），组内应用归属同步改写；重名/空名/不存在返回 false */
  const renameGroup = (oldName: string, newName: string): boolean => {
    const trimmed = newName.trim();
    if (!groups.value.includes(oldName) || !trimmed || groups.value.includes(trimmed)) {
      return false;
    }
    groups.value = groups.value.map((group) => (group === oldName ? trimmed : group));
    assignments.value = Object.fromEntries(
      Object.entries(assignments.value).map(([id, group]) => [id, group === oldName ? trimmed : group])
    );
    save();
    return true;
  };

  /** 删除分组（至少保留一个）；组内应用清除归属，回落到回退分组 */
  const deleteGroup = (name: string): boolean => {
    if (!groups.value.includes(name) || groups.value.length <= 1) {
      return false;
    }
    groups.value = groups.value.filter((group) => group !== name);
    assignments.value = Object.fromEntries(
      Object.entries(assignments.value).filter(([, group]) => group !== name)
    );
    save();
    return true;
  };

  return { allGroups, groupOf, moveToGroup, orderOf, persistOrder, createGroup, renameGroup, deleteGroup };
}

// 「最近使用」自动分组：localStorage 记录打开时间（§2.3/§7.1，mock 性质但真实可用）
export function useRecentApps(spaceKey: Ref<string>) {
  const recentIds = ref<string[]>([]);
  const storageKey = computed(() => `spark:apps-recent:${spaceKey.value}`);

  const load = () => {
    recentIds.value = loadJson<string[]>(storageKey.value, []);
  };
  watch(spaceKey, load, { immediate: true });

  const recordOpen = (pluginId: string) => {
    recentIds.value = [pluginId, ...recentIds.value.filter((id) => id !== pluginId)].slice(0, 4);
    saveJson(storageKey.value, recentIds.value);
  };

  return { recentIds, recordOpen };
}

// TODO(mock): 组织空间应用启用状态内核没有接口（设计 §4.2 由管理员统一管理），先按 orgId 存 localStorage
export function useOrgEnabled(spaceKey: Ref<string>) {
  const enabledMap = ref<Record<string, boolean>>({});
  const storageKey = computed(() => `spark:apps-org-enabled:${spaceKey.value}`);

  const load = () => {
    enabledMap.value = loadJson<Record<string, boolean>>(storageKey.value, {});
  };
  watch(spaceKey, load, { immediate: true });

  const isOrgEnabled = (pluginId: string): boolean => enabledMap.value[pluginId] ?? false;

  const setOrgEnabled = (pluginId: string, enabled: boolean) => {
    enabledMap.value = { ...enabledMap.value, [pluginId]: enabled };
    saveJson(storageKey.value, enabledMap.value);
  };

  return { isOrgEnabled, setOrgEnabled };
}
