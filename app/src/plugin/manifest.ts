/**
 * 壳层 manifest 视图线形校验（A56：background 视图类型合法化）。
 *
 * 视图类型三种合法线形：
 * - `app`：主视图（全页/窗口 iframe，能力面全量、仅 grantedPermissions 过滤）；
 * - `message-card`：消息卡片（聊天内限定区域 iframe，能力面按 view 裁剪）；
 * - `background`：后台视图（无 UI 面）——声明即接入内核 QuickJS 后台运行时，
 *   脚本入口由 manifest 顶层 `background` 字段给出（内核 plugin_runtime 对账
 *   读取的契约字段），生命周期按既有惯例对账：插件启用即拉起常驻线程、
 *   停用/卸载即销毁。
 *
 * 配对规则：
 * - 声明 background 视图必须配套顶层 `background` 入口字段（否则无脚本可跑，
 *   声明即空诺，fail-closed 拒绝）；
 * - 反向不强制：顶层 `background` 入口而无 background 视图声明为历史线形
 *   （ai-chat 先例），兼容放行；
 * - `entryView` 必须存在于 views 且不得指向 background 视图（无 UI 面不可打开）。
 *
 * 校验失败抛 `InvalidManifest: ...`；`fetchPluginManifest` 捕获后按
 * 「无 manifest」降级（返回 null），与读取失败同口径。
 */
import type { PluginManifest } from '../../../packages/plugin-sdk/src';

/** 合法视图类型白名单（与 PluginViewDeclaration.type 逐字对齐） */
export const PLUGIN_VIEW_TYPES = ['app', 'message-card', 'background'] as const;

export type PluginViewTypeName = (typeof PLUGIN_VIEW_TYPES)[number];

export function isPluginViewType(type: unknown): type is PluginViewTypeName {
  return typeof type === 'string' && (PLUGIN_VIEW_TYPES as readonly string[]).includes(type);
}

function fail(reason: string): never {
  throw new Error(`InvalidManifest: ${reason}`);
}

/**
 * 校验 manifest 的视图线形（views / entryView / background 三者的一致性）。
 * 合法返回原 manifest；非法抛 InvalidManifest。只校验本任务线形相关字段，
 * 其余字段（permissions/requires 等）的信任边界在内核与市场侧。
 */
export function validatePluginManifest(manifest: PluginManifest): PluginManifest {
  const rawViews = (manifest as { views?: unknown }).views;
  if (!Array.isArray(rawViews)) {
    fail('views must be an array');
  }
  const views = rawViews as Array<{ id?: unknown; type?: unknown }>;
  let hasBackgroundView = false;
  for (const view of views) {
    if (typeof view !== 'object' || view === null || typeof view.id !== 'string' || view.id.length === 0) {
      fail('each view must declare a non-empty string id');
    }
    if (!isPluginViewType(view.type)) {
      fail(`view "${view.id}" has unknown type "${String(view.type)}" (expected ${PLUGIN_VIEW_TYPES.join(' / ')})`);
    }
    if (view.type === 'background') {
      hasBackgroundView = true;
    }
  }
  // background 视图无 UI 面、实例跑在内核 QuickJS 运行时：脚本入口必须经
  // 顶层 background 字段给出（内核对账只认该字段），否则声明即空诺
  if (hasBackgroundView) {
    const entry = (manifest as { background?: unknown }).background;
    if (typeof entry !== 'string' || entry.length === 0) {
      fail('a background view requires the top-level "background" entry field (kernel QuickJS runtime entry)');
    }
  }
  // entryView 必须存在且可打开：background 视图无 UI 面，不能作为入口
  const entryView = (manifest as { entryView?: unknown }).entryView;
  if (entryView !== undefined) {
    if (typeof entryView !== 'string') {
      fail('entryView must be a string');
    }
    const entry = views.find((view) => view.id === entryView);
    if (!entry) {
      fail(`entryView "${entryView}" is not declared in views`);
    }
    if (entry.type === 'background') {
      fail(`entryView "${entryView}" must not be a background view (no UI to open)`);
    }
  }
  return manifest;
}
