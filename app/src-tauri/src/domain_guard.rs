//! 命令层系统域守卫（旧 TS `requireSystemDomain(event)` 的 Tauri 等价物）。
//!
//! 背景与边界（务必如实理解）：
//! - Tauri 2 的 `InvokeRequest.url` 恒为 `ipc://localhost/{cmd}`，**不携带**调用方
//!   来源信息；`Webview::url()` 返回的是**顶层 webview 的 URL**，不是发起调用的
//!   frame 的 URL。
//! - 当前架构插件跑在 `sandbox="allow-scripts"`（opaque origin、无
//!   `allow-same-origin`）的 srcdoc iframe 内，与系统域**同一 webview**：
//!   - 插件 iframe 因 opaque origin 无法访问父级 `window.__TAURI_INTERNALS__`，
//!     不能直接 invoke 命令（Tauri 只在顶层注入 IPC 能力）；
//!   - 因此本守卫无法也无需在「同 webview 的 iframe」维度做区分（那由 iframe
//!     沙箱 + 桥权限中间件 `plugin-bridge-dispatcher.ts` 保证）。
//! - 本守卫防御的是**独立插件窗口**：`plugin-open-view` 排期落地后，插件窗口是
//!   独立 webview，其 `url()` 为 `plugin://localhost/<pluginId>/...`（或 Windows
//!   `http://plugin.localhost`），与系统域同进程可 invoke 任意命令。本守卫
//!   fail-closed：只放行系统域 URL，插件源一律拒绝，为域隔离提前落地安全边界。
//!
//! 用法：市场等「仅系统域可用」的命令在进入业务逻辑前调用
//! `require_system_domain(&webview)?`。
//!
//! 设计：判定逻辑抽成纯函数 [`is_system_origin`]，单测无需 tauri `test` feature；
//! [`require_system_domain`] 只是 `&Webview` → URL 的薄包装。

use tauri::Webview;

/// 系统域判定纯函数：`(scheme, host, port)` 是否属系统域白名单。
///
/// 白名单（对齐 tauri.conf.json `devUrl` 与 tauri 生产自定义协议形态）：
/// - 开发：`http://localhost:1420`（tauri.conf.json `devUrl`，vite dev server）；
/// - 生产：`tauri://localhost` / `http(s)://tauri.localhost`（tauri 默认自定义协议，
///   Windows 为 http、移动端为 https、桌面为 tauri://）；
/// - 其它 `localhost` 端口（dev-multi.mjs 多开场景）一并放行；
/// - `plugin://`、`http(s)://plugin.localhost`（插件源 workaround）**一律拒绝**。
pub fn is_system_origin(scheme: &str, host: &str, _port: Option<u16>) -> bool {
    // 插件源前缀（独立插件窗口 / Windows-Android 插件源 workaround）——fail-closed
    if scheme == "plugin" || (matches!(scheme, "http" | "https") && host == "plugin.localhost") {
        return false;
    }
    matches!(
        (scheme, host),
        ("tauri", "localhost")
            | ("https", "tauri.localhost")
            | ("http", "tauri.localhost")
            | ("http", "localhost")
            | ("https", "localhost")
    )
}

/// 系统域判定：调用方 webview 的当前 URL 属系统域白名单。
///
/// 语义：系统域放行 `Ok(())`；未知/插件源 `Err(msg)`（fail-closed，文案供前端展示）。
pub fn require_system_domain(webview: &Webview) -> Result<(), String> {
    let url = webview.url().map_err(|e| {
        format!(
            "This command is restricted to the system domain (webview url unavailable: {e})"
        )
    })?;
    if is_system_origin(url.scheme(), url.host_str().unwrap_or(""), url.port()) {
        Ok(())
    } else {
        Err(format!(
            "This command is restricted to the system domain (caller origin: {}://{})",
            url.scheme(),
            url.host_str().unwrap_or("")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plugin_scheme() {
        assert!(!is_system_origin("plugin", "localhost", None));
    }

    #[test]
    fn rejects_plugin_localhost_workaround() {
        assert!(!is_system_origin("http", "plugin.localhost", Some(80)));
        assert!(!is_system_origin("https", "plugin.localhost", None));
    }

    #[test]
    fn rejects_unknown_remote_hosts() {
        // 未知远程主机（如未来插件窗口误用 http://evil）一律拒绝
        assert!(!is_system_origin("https", "github.com", None));
        assert!(!is_system_origin("file", "", None));
    }

    #[test]
    fn accepts_dev_url() {
        assert!(is_system_origin("http", "localhost", Some(1420)));
    }

    #[test]
    fn accepts_tauri_production_scheme() {
        assert!(is_system_origin("tauri", "localhost", None));
        assert!(is_system_origin("https", "tauri.localhost", None));
        assert!(is_system_origin("http", "tauri.localhost", None));
    }
}
