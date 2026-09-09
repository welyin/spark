/**
 * PC 空间桌面 · 应用注册表（阶段 2 / ui-architecture §4.2 两级状态分离之「应用注册表」）。
 *
 * 当前空间已装插件 → Map<appId, AppDef>。数据源 = pluginMarket.list() 过滤
 * 「installed && 本空间可见（supportedSpaces）」，与 AppsPage / SpaceDesktop 同口径。
 * 模块级单例 ref，按当前空间（current-space）变化自动重建。
 *
 * appId 取插件 id（仓库规范化地址），一插件一应用；窗口实例（多开）由
 * window-manager.ts 的 winKey 管理，本表只描述「有哪些应用可开」。
 */
import { computed, ref, watch } from 'vue';
import type { PluginMarketItemDto } from '../../api/types';
import { currentSpace } from '../current-space';
import { isPluginVisibleInSpace } from '../../components/apps/space-visibility';
import { listMockApps } from '../../mock/apps';
import { mockMode } from '../../mock/mode';

/** 应用定义：桌面图标与开窗所需的最小描述（参照 ark AppDef，载体恒为插件 iframe） */
export interface AppDef {
  /** 应用 id（= 插件 id） */
  id: string;
  /** 插件域（plugin:<id>，开窗时 pluginId = domain 去前缀） */
  pluginDomain: string;
  /** 名称（图标下文字 / 窗口标题） */
  name: string;
  /** 图标文本（首字符，图标着色由 appIconBackground 按 id 哈希） */
  icon: string;
  /** 默认打开的视图（manifest views[0]，缺省 'default'） */
  view: string;
  /** 初始窗口尺寸（px；缺省 880×620，可被 manifest `window` 声明覆盖） */
  defaultWidth: number;
  defaultHeight: number;
}

/** 窗口默认尺寸（plugin-dist §2.1：未声明或非法值一律回退 880×620） */
const DEFAULT_WINDOW_WIDTH = 880;
const DEFAULT_WINDOW_HEIGHT = 620;
/** 窗口声明合法范围（与壳层 market catalog normalize_window 同口径；
 *  下限即 WindowFrame 最小夹取 320×220，上限 4K） */
const WINDOW_MIN_WIDTH = 320;
const WINDOW_MAX_WIDTH = 3840;
const WINDOW_MIN_HEIGHT = 220;
const WINDOW_MAX_HEIGHT = 2160;

/** manifest `window` 声明 → 初始窗口尺寸；缺省/越界/非整数回退默认。
 *  壳层 DTO 已归一化，此处为桥边界后的二次防卫（mock 数据与手改状态文件不经过壳层归一化） */
function resolveWindowSize(item: PluginMarketItemDto): { width: number; height: number } {
  const spec = item.window;
  if (
    spec &&
    Number.isInteger(spec.defaultWidth) &&
    spec.defaultWidth >= WINDOW_MIN_WIDTH &&
    spec.defaultWidth <= WINDOW_MAX_WIDTH &&
    Number.isInteger(spec.defaultHeight) &&
    spec.defaultHeight >= WINDOW_MIN_HEIGHT &&
    spec.defaultHeight <= WINDOW_MAX_HEIGHT
  ) {
    return { width: spec.defaultWidth, height: spec.defaultHeight };
  }
  return { width: DEFAULT_WINDOW_WIDTH, height: DEFAULT_WINDOW_HEIGHT };
}

/** 市场条目 → AppDef */
function toAppDef(item: PluginMarketItemDto): AppDef {
  const size = resolveWindowSize(item);
  return {
    id: item.id,
    pluginDomain: item.domain,
    name: item.name,
    icon: item.name.slice(0, 1),
    view: item.views[0] ?? 'default',
    defaultWidth: size.width,
    defaultHeight: size.height
  };
}

/** 全部已装插件（原始清单，未按空间过滤） */
const allItems = ref<PluginMarketItemDto[]>([]);
const loadError = ref('');

/** 当前空间 id（个人空间 'personal' / 组织 orgId）：注册表按键重建与查询的作用域 */
export const desktopSpaceId = computed<string>(() =>
  currentSpace.value.type === 'org' ? currentSpace.value.orgId : 'personal'
);

async function refresh(): Promise<void> {
  try {
    allItems.value = [...await window.electronAPI.pluginMarket.list()];
    loadError.value = '';
  } catch (err) {
    loadError.value = `加载应用失败：${err}`;
  }
}

/** 应用注册表：当前空间已装且本空间可见的应用（Map<appId, AppDef>），随空间切换重建 */
export const appRegistry = computed<Map<string, AppDef>>(() => {
  const map = new Map<string, AppDef>();
  const items = new Map([
    ...(mockMode() ? listMockApps() : []),
    ...allItems.value
  ].map((item) => [item.id, item]));
  for (const item of items.values()) {
    if (!item.installed) {
      continue;
    }
    if (!isPluginVisibleInSpace(item.supportedSpaces, currentSpace.value.type)) {
      continue;
    }
    map.set(item.id, toAppDef(item));
  }
  return map;
});

/** 注册表有序列表（桌面图标网格渲染用；缺省按名称序，排布持久化由桌面层覆盖） */
export const appList = computed<AppDef[]>(() =>
  [...appRegistry.value.values()].sort((a, b) => a.name.localeCompare(b.name, 'zh'))
);

export const appRegistryError = computed(() => loadError.value);

/** 内置壳层窗口（非插件）：全局页面在任意空间桌面以窗口形态打开 */
const SHELL_APPS: Record<string, AppDef> = {
  'spark:market': { id: 'spark:market', pluginDomain: '', name: '应用市场', icon: '市', view: '', defaultWidth: 1000, defaultHeight: 680 },
  'spark:messages': { id: 'spark:messages', pluginDomain: '', name: '全部消息', icon: '讯', view: '', defaultWidth: 1000, defaultHeight: 680 },
  'spark:affairs': { id: 'spark:affairs', pluginDomain: '', name: '所有事务', icon: '事', view: '', defaultWidth: 960, defaultHeight: 660 },
  'spark:mine': { id: 'spark:mine', pluginDomain: '', name: '我的', icon: '我', view: '', defaultWidth: 1000, defaultHeight: 680 },
  'spark:settings': { id: 'spark:settings', pluginDomain: '', name: '设置', icon: '设', view: '', defaultWidth: 1000, defaultHeight: 680 },
  'spark:test': { id: 'spark:test', pluginDomain: '', name: '测试', icon: '测', view: '', defaultWidth: 960, defaultHeight: 660 }
};

/** 按 appId 查应用定义（开窗时取标题/尺寸/视图） */
export function getApp(appId: string, spaceType = currentSpace.value.type): AppDef | null {
  if (SHELL_APPS[appId]) {
    return SHELL_APPS[appId];
  }
  const item = allItems.value.find((entry) => entry.id === appId)
    ?? (mockMode() ? listMockApps().find((entry) => entry.id === appId) : undefined);
  return item?.installed && isPluginVisibleInSpace(item.supportedSpaces, spaceType) ? toAppDef(item) : null;
}

/** 手动刷新（安装/卸载/启停后由调用方触发；桌面挂载时也会调一次兜底） */
export function refreshAppRegistry(): Promise<void> {
  return refresh();
}

// 模块加载即拉一次；electronAPI 未就绪（RootGate 阶段 import）时首拉可能为空，
// 真正的兜底刷新由桌面组件挂载时调用 refreshAppRegistry 完成（见 PcDesktop.onMounted）。
void refresh();
watch(desktopSpaceId, () => {
  // 空间切换不改变已装清单（installed 是全局的），注册表 computed 自动重建；
  // 此处仅兜底重新拉取以拿到最新安装态（如另一窗口刚装/卸）。
  void refresh();
});
