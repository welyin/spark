/**
 * 插件源 URL（壳层侧）：与 src-tauri plugin:// 协议 / vite dev 中间件对应的资源定位。
 *
 * 跨平台形态（与 Tauri convertFileSrc 的 asset 协议同款 workaround，见
 * src-tauri/src/plugin_src.rs 与 wry custom_protocol_workaround）：
 * - macOS/Linux：自定义 scheme 原生可用，`plugin://localhost/<pluginId>/<path>`；
 * - Windows/Android：WebView2 不拦截非标准 scheme 的子资源请求，wry 只拦截
 *   `http(s)://plugin.*`，必须直接引用 `http://plugin.localhost/<pluginId>/<path>`；
 * - 纯浏览器 dev（非 Tauri）：vite 中间件 `/plugin/<pluginId>/<path>`，与壳层同 origin
 *   （dev 下同 origin、无 OOPIF；沙箱 iframe 仍得 opaque origin，桥握手两端一致）。
 *
 * 宿主 HTML 采用 iframe srcdoc 内联生成（任务书推荐方案）：bundle 与 css 经上述
 * 插件源加载，CSP 经 srcdoc meta 与源服务响应头双重施加。srcdoc 为 opaque origin，
 * CSP 'self' 不匹配任何 URL，必须显式列插件源来源（见 buildPluginHostSrcdoc）。
 */

import { isTauri } from './api';
import type { PluginManifest, PluginViewBootstrap } from '../../packages/plugin-sdk/src';

/** 插件 id 白名单（与内核 §20 规格一致）：小写字母/数字/连字符，首字符非连字符，最长 64 */
const PLUGIN_ID_PATTERN = /^[a-z0-9][a-z0-9-]{0,63}$/;

/** 仓库锚定 id（plugin-dist §1）：{host}/{owner}/{repo}[/path]，host 限三大托管平台 */
const REPO_ID_PATTERN =
  /^(github\.com|gitlab\.com|gitee\.com)(\/[a-z0-9._-]{1,100}){2}(\/[a-z0-9._-]{1,64}){0,8}$/;

export function isValidPluginId(pluginId: string): boolean {
  return PLUGIN_ID_PATTERN.test(pluginId) || REPO_ID_PATTERN.test(pluginId);
}

/** 入口校验：pluginId 将拼进 srcdoc/URL 路径段，不合规直接拒绝（防注入） */
function assertValidPluginId(pluginId: string): void {
  if (!isValidPluginId(pluginId)) {
    throw new Error(`Invalid plugin id: ${pluginId}`);
  }
}

/** 插件源基址（不含尾部斜杠）；repo id 含 `/`，经 encodeURIComponent 收成单段传输，
 *  源服务（plugin_src.rs / vite dev 中间件）解码还原；旧短名 id 编码后为恒等 */
export function pluginSourceBaseUrl(pluginId: string): string {
  assertValidPluginId(pluginId);
  const encoded = encodeURIComponent(pluginId);
  if (!isTauri()) {
    return `${window.location.origin}/plugin/${encoded}`;
  }
  const isWindows = navigator.userAgent.includes('Windows');
  return isWindows ? `http://plugin.localhost/${encoded}` : `plugin://localhost/${encoded}`;
}

/** 源服务统一 CSP 口径（与 plugin_src.rs PLUGIN_CSP 一致）；script-src 由变量拼接 */
const sourceCsp = (scriptSrc: string): string =>
  `default-src 'self'; script-src ${scriptSrc}; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:`;

/**
 * 生成宿主 iframe 的 srcdoc：`<div id="app"></div>` + module bundle + 样式
 * （assets/main.css 缺失时 404 静默忽略，不阻断加载）。
 *
 * CSP 经 meta 施加（iframe 外层一重；源服务响应头为另一重）：
 * - Tauri：srcdoc opaque origin 下 'self' 无效，显式列插件源 origin；
 * - 纯浏览器 dev：与壳层同 origin，'self' 即插件源。
 *
 * mount（视图引导信息）：内联脚本注入 `window.__sparkPluginView`——插件握手前
 * 唯一能拿到 viewId/卡片上下文的途径（hello 的 viewId 必须与桥绑定一致，
 * 插件无法可靠自报时由注入兜底，见 bridge/client.ts）；JSON 转义 `<` 防 `</script>` 逃逸。
 */
export function buildPluginHostSrcdoc(pluginId: string, mount?: PluginViewBootstrap): string {
  assertValidPluginId(pluginId);
  const base = pluginSourceBaseUrl(pluginId);
  // mount 引导脚本为内联 script：仅注入 mount 时给 script-src 追加 'unsafe-inline'
  // 放行（iframe 沙箱内插件 bundle 本就是任意代码，CSP 的网络外联约束不受此影响；
  // 主视图不传 mount 则 CSP 维持原口径）
  const scriptSrc = mount ? `${base} 'unsafe-inline'` : base;
  const csp = isTauri()
    ? `default-src 'none'; script-src ${scriptSrc}; style-src ${base} 'unsafe-inline'; connect-src ${base}; img-src ${base} data:; font-src ${base}`
    : sourceCsp(mount ? `'self' 'unsafe-inline'` : `'self'`);
  const mountScript = mount
    ? `<script>window.__sparkPluginView = ${JSON.stringify(mount).replace(/</g, '\\u003c')};</script>`
    : '';
  return [
    '<!doctype html>',
    '<html>',
    '<head>',
    '<meta charset="utf-8" />',
    `<meta http-equiv="Content-Security-Policy" content="${csp}" />`,
    `<link rel="stylesheet" href="${base}/assets/main.css" />`,
    mountScript,
    '</head>',
    '<body>',
    '<div id="app"></div>',
    `<script type="module" src="${base}/views/main.js"></script>`,
    '</body>',
    '</html>'
  ].join('\n');
}

/**
 * 读插件 manifest（best-effort）：宿主组装 ctx（supportedSpaces、显示名）用；
 * 读取失败返回 null，调用方按「无 manifest」降级处理。
 */
export async function fetchPluginManifest(pluginId: string): Promise<PluginManifest | null> {
  try {
    const response = await fetch(`${pluginSourceBaseUrl(pluginId)}/manifest.json`);
    if (!response.ok) {
      return null;
    }
    return (await response.json()) as PluginManifest;
  } catch {
    return null;
  }
}
