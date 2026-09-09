/**
 * 默认内置应用灰度（communication §4.2 / §五.3，A19）：聊天界面在「旧内置 UI」
 * 与「默认内置插件版」之间切换。并存一个版本，用户无感切换后旧 UI 在后续版本
 * 移除（移除动作不在本期）。
 *
 * - 数据源同一套（消息在 sled，插件经 sdk.messages 访问），切换实现零迁移、
 *   数据原样在；
 * - 选择持久化在 localStorage（每 tab 独立键），默认 legacy（旧内置 UI）；
 * - 插件版经 PluginIframeHost 挂载（与插件 tab 同一沙箱链路）；加载失败
 *   「关闭」回退 legacy（App.vue onBuiltinPluginClose）。
 *
 * 注：通讯录已转为空间桌面插件窗口（docs/ui 阶段 3，经 openPluginTab 全屏
 * 打开 spark-contacts），不再是壳层主 tab，无灰度面，故不在注册表内。
 */
import { ref } from 'vue';

export type BuiltinImpl = 'legacy' | 'plugin';

export interface BuiltinAppDef {
  /** 壳层主 tab id（'messages'） */
  tabId: string;
  /** 默认内置插件 id（壳层预装，市场安装状态由 src-tauri 首跑写入） */
  pluginId: string;
  /** 入口视图（manifest.entryView） */
  viewId: string;
}

/** 默认内置插件注册表（壳层侧唯一事实源；插件工程在 code/plugins/<id>） */
export const BUILTIN_APPS: BuiltinAppDef[] = [
  { tabId: 'messages', pluginId: 'spark-chat', viewId: 'default' }
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
