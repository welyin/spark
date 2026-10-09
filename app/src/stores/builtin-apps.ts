/**
 * 默认内置应用灰度（communication §4.2 / §五.3，A19/A34）：聊天与市场界面在
 * 「旧内置 UI」与「默认内置插件版」之间切换。并存一个版本，用户无感切换后旧 UI
 * 在后续版本移除（移除动作不在本期）。
 *
 * - 数据源同一套（消息在 sled；市场安装状态在内核市场服务，插件经 sdk.market
 *   访问），切换实现零迁移、数据原样在；
 * - 选择持久化在 localStorage（每 tab 独立键），默认 legacy（旧内置 UI）；
 * - 插件版经 PluginIframeHost 挂载（与插件 tab 同一沙箱链路）；加载失败
 *   「关闭」回退 legacy（App.vue onBuiltinPluginClose）。
 *
 * 注：通讯录已转为空间桌面插件窗口（docs/ui 阶段 3，经 openPluginTab 全屏
 * 打开 spark-contacts），不再是壳层主 tab，无灰度面，故不在注册表内。
 * 市场（A34）：'apps' tab（应用管理）与桌面窗口 spark:market 共用同一注册项；
 * 空间的应用市场（spark:space-market，per-space 启停依赖壳层 app-enablement
 * 事实源）本期不迁移，恒 legacy（见任务报告遗留）。
 * 组织管理（A42）：'org' 不是主 tab——组织设置界面的宿主是顶栏空间设置对话框
 * （SpaceSettingsDialog）与设置页组织空间栏（SettingsPage 'space' 菜单），
 * 两处均经 BuiltinAppHost 灰度；组织创建/加入对话框（MembershipDialogs）
 * 是壳层入口动作不在灰度面（插件版内含创建/加入面板，两通路并存一个版本）。
 */
import { ref } from 'vue';

export type BuiltinImpl = 'legacy' | 'plugin';

export interface BuiltinAppDef {
  /** 壳层主 tab id（'messages' / 'apps'） */
  tabId: string;
  /** 默认内置插件 id（壳层预装，市场安装状态由 src-tauri 首跑写入） */
  pluginId: string;
  /** 入口视图（manifest.entryView） */
  viewId: string;
}

/** 默认内置插件注册表（壳层侧唯一事实源；插件工程在 code/plugins/<id>） */
export const BUILTIN_APPS: BuiltinAppDef[] = [
  { tabId: 'messages', pluginId: 'spark-chat', viewId: 'default' },
  { tabId: 'apps', pluginId: 'spark-market', viewId: 'default' },
  // A42 组织管理：非主 tab——宿主为 SpaceSettingsDialog 与 SettingsPage 组织栏
  { tabId: 'org', pluginId: 'spark-org-admin', viewId: 'default' }
];

const storageKey = (tabId: string): string => `spark:builtin-impl:${tabId}`;

function readImpl(tabId: string): BuiltinImpl {
  try {
    return localStorage.getItem(storageKey(tabId)) === 'plugin' ? 'plugin' : 'legacy';
  } catch {
    // localStorage 不可用（隐私模式等）按默认 legacy
    return 'legacy';
  }
}

/** 响应式实现选择（模板渲染期间读取即被追踪；初值读 localStorage） */
const impls = ref<Record<string, BuiltinImpl>>(
  Object.fromEntries(BUILTIN_APPS.map((def) => [def.tabId, readImpl(def.tabId)]))
);

/** 某主 tab 当前界面实现（未登记 tab 恒 legacy） */
export function builtinImpl(tabId: string): BuiltinImpl {
  return impls.value[tabId] ?? 'legacy';
}

/** 切换界面实现并持久化（legacy=旧内置 UI / plugin=默认内置插件版） */
export function setBuiltinImpl(tabId: string, impl: BuiltinImpl): void {
  if (!BUILTIN_APPS.some((def) => def.tabId === tabId)) {
    return;
  }
  impls.value = { ...impls.value, [tabId]: impl };
  try {
    localStorage.setItem(storageKey(tabId), impl);
  } catch {
    // 持久化失败仅本次会话生效，不阻断切换
  }
}

/** tab → 默认内置插件定义（非内置 tab 返回 undefined） */
export function builtinPluginFor(tabId: string): BuiltinAppDef | undefined {
  return BUILTIN_APPS.find((def) => def.tabId === tabId);
}
