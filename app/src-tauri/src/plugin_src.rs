//! 插件源服务：`plugin://` 自定义协议的资源解析（插件 iframe 沙箱化阶段 A 第二波）。
//!
//! URL 形如 `plugin://<pluginId>/<path>`，解析顺序先安装包后内置 dist：
//! - 已安装包：`<app_data_dir>/plugins/<id>/packages/*.spkg`（.spkg = JSON 容器
//!   `{pluginId, domain, version, files:[{path, sha256, size, contentBase64}]}`，
//!   见 code/plugins/scripts/build-example-package.mjs）。定位只信
//!   plugin-market-state.json 记录的 packagePath（fail-closed：无记录、状态
//!   文件存在但解析失败、`enabled = false`，一律拒服，不做目录扫描猜测）。
//! - 内置开发插件 dist：`code/plugins/<id>/dist/<path>`（编译期
//!   CARGO_MANIFEST_DIR 与运行时 cwd 双候选，对齐 market::MarketPaths::for_app）。
//!
//! 安全约束：
//! - 路径穿越防护：rel path 逐段校验，拒绝 `..`、反斜杠与空段；
//! - 全部响应带 CSP 头（PLUGIN_CSP；源服务一重施加，宿主 iframe 外层另有一重）；
//! - 带 `Access-Control-Allow-Origin: *`：沙箱 iframe（opaque origin）以 CORS 模式
//!   拉取 module bundle 的必需头；plugin:// 仅在本应用 WebView 内可达，暴露面可控。
//!
//! 性能说明：.spkg 逐请求解析（无缓存）。bundle 约 1.6MB（base64 ~2.2MB），
//! 每实例挂载仅加载一次，可接受；如需缓存待内核侧统一考虑。

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::Deserialize;
use tauri::http::{header, Response, StatusCode};

use crate::market::types::PersistedPluginState;

/// 源服务统一 CSP（任务书口径；与宿主 iframe 外层施加的策略一致）
pub const PLUGIN_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:";

// ------------------------------------------------------------------
// 路径与 MIME（纯函数，单测覆盖）
// ------------------------------------------------------------------

/// 按扩展名推断 MIME（module script 必须是 JS MIME，否则浏览器拒载）。
fn mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// 路径段校验：插件 id 与 rel path 共用——拒绝 `..`、反斜杠、空段与绝对路径。
/// 返回规范化后的段序列（不含空段）；任何非法输入返回 None。
fn sanitize_segments(raw: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." {
            // 空段（前导/连续斜杠）与 "." 折叠；纯前导斜杠是 URL path 常态
            continue;
        }
        if segment == ".." || segment.contains('\\') {
            return None;
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments)
}

// ------------------------------------------------------------------
// .spkg 容器读取
// ------------------------------------------------------------------

#[derive(Deserialize)]
struct SpkgFileEntry {
    path: String,
    #[serde(rename = "contentBase64")]
    content_base64: String,
}

#[derive(Deserialize)]
struct SpkgContainer {
    files: Vec<SpkgFileEntry>,
}

/// 从 .spkg 容器内取单个文件（path 精确匹配，命中即解码 base64）。
fn read_from_spkg(spkg_path: &Path, rel_path: &str) -> Option<Vec<u8>> {
    let text = fs::read_to_string(spkg_path).ok()?;
    let container: SpkgContainer = serde_json::from_str(&text).ok()?;
    let entry = container.files.into_iter().find(|f| f.path == rel_path)?;
    base64::engine::general_purpose::STANDARD
        .decode(entry.content_base64)
        .ok()
}

/// 定位已安装插件的 .spkg：只信 plugin-market-state.json 记录的 packagePath
/// （fail-closed：无记录、状态文件存在但解析失败、enabled = false、记录路径
/// 缺失或不是 .spkg，一律拒服——不做 packages 目录扫描猜测，避免字典序取错版本）。
fn locate_installed_spkg(data_dir: &Path, plugin_id: &str) -> Option<PathBuf> {
    let state_text = fs::read_to_string(data_dir.join("plugin-market-state.json")).ok()?;
    let state: PersistedPluginState = serde_json::from_str(&state_text).ok()?;
    let installed = state.installed.get(plugin_id)?;
    if !installed.enabled {
        return None;
    }
    let recorded = PathBuf::from(&installed.package_path);
    if recorded.extension().is_some_and(|ext| ext == "spkg") && recorded.is_file() {
        return Some(recorded);
    }
    None
}

/// 内置开发插件 dist 候选根（对齐 market::MarketPaths::for_app 的 source_roots 语义）。
fn builtin_dist_roots() -> Vec<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cwd = std::env::current_dir().unwrap_or_default();
    vec![
        manifest_dir.join("../../plugins"),
        cwd.join("../plugins"),
    ]
}

// ------------------------------------------------------------------
// 资源解析
// ------------------------------------------------------------------

/// 解析插件资源：先安装包后内置 dist，返回（字节, MIME）。
pub fn resolve_plugin_resource(data_dir: &Path, plugin_id: &str, rel_path: &str) -> Option<(Vec<u8>, &'static str)> {
    let id_segments = sanitize_segments(plugin_id)?;
    let path_segments = sanitize_segments(rel_path)?;
    let rel_normalized = path_segments.join("/");
    let mime = mime_for_path(&rel_normalized);

    // 1) 已安装包（.spkg 容器）
    if let Some(spkg) = locate_installed_spkg(data_dir, &id_segments.join("/")) {
        if let Some(bytes) = read_from_spkg(&spkg, &rel_normalized) {
            return Some((bytes, mime));
        }
    }

    // 2) 内置开发插件 dist
    for root in builtin_dist_roots() {
        let mut candidate = root;
        for segment in &id_segments {
            candidate = candidate.join(segment);
        }
        candidate = candidate.join("dist");
        for segment in &path_segments {
            candidate = candidate.join(segment);
        }
        if candidate.is_file() {
            if let Ok(bytes) = fs::read(&candidate) {
                return Some((bytes, mime));
            }
        }
    }

    None
}

// ------------------------------------------------------------------
// 协议入口（register_uri_scheme_protocol 回调）
// ------------------------------------------------------------------

fn respond(status: StatusCode, mime: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_SECURITY_POLICY, PLUGIN_CSP)
        // 沙箱 iframe 为 opaque origin，module script 走 CORS：必须显式放行
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(body)
        .expect("plugin:// response build failed")
}

/// 百分号解码 UTF-8（仓库锚定 repo id 含 `/`，前端经 encodeURIComponent 收成
/// 单段传输；非法 % 序列 / 非 UTF-8 返回 None）。
/// 只作用于 pluginId 段：rel path 保持既有行为不解码。
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 解析插件资源 URL。
///
/// 跨平台 URL 形态（与 Tauri asset 协议同款 workaround，见 wry
/// custom_protocol_workaround）：
/// - macOS/Linux/iOS：自定义 scheme 原生可用，`plugin://localhost/<pluginId>/<path>`；
/// - Windows/Android：WebView2 不拦截非标准 scheme 的子资源请求，wry 只拦截
///   `http(s)://plugin.*`，页面必须直接引用 `http://plugin.localhost/<pluginId>/<path>`，
///   wry 拦截后 revert 为 `plugin://localhost/<pluginId>/<path>` 交给本函数。
/// 因此本函数统一按「host = localhost 时首段路径为 pluginId」解析；
/// host 形式 `plugin://<pluginId>/<path>`（非 Windows 手写出）也兼容受理。
pub fn handle_plugin_request(data_dir: &Path, uri: &tauri::http::Uri) -> Response<Vec<u8>> {
    let host = uri.host().unwrap_or("").to_ascii_lowercase();
    let raw_path = uri.path().trim_start_matches('/');

    let (plugin_id, rel_path) = if host.is_empty() || host == "localhost" {
        // 规范形态：plugin://localhost/<pluginId>/<path>
        let mut parts = raw_path.splitn(2, '/');
        let id = parts.next().unwrap_or("").to_string();
        (id, parts.next().unwrap_or("").to_string())
    } else {
        // 兼容形态：plugin://<pluginId>/<path>
        (host.clone(), raw_path.to_string())
    };

    // repo id 经 encodeURIComponent 传输（plugin-source.ts），此处还原；
    // 解码后的段校验在 resolve_plugin_resource 的 sanitize_segments 完成
    let Some(plugin_id) = percent_decode(&plugin_id) else {
        return respond(
            StatusCode::BAD_REQUEST,
            "text/plain; charset=utf-8",
            b"plugin:// invalid percent-encoded plugin id".to_vec(),
        );
    };

    if plugin_id.is_empty() || rel_path.is_empty() {
        return respond(
            StatusCode::BAD_REQUEST,
            "text/plain; charset=utf-8",
            b"plugin:// requires plugin://localhost/<pluginId>/<path>".to_vec(),
        );
    }

    match resolve_plugin_resource(data_dir, &plugin_id, &rel_path) {
        Some((bytes, mime)) => respond(StatusCode::OK, mime, bytes),
        None => respond(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            format!("plugin resource not found: {plugin_id}/{rel_path}").into_bytes(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_segments("../etc/passwd").is_none());
        assert!(sanitize_segments("views/../../x").is_none());
        assert!(sanitize_segments("views\\..\\x").is_none());
        assert!(sanitize_segments("").is_none());
        assert_eq!(sanitize_segments("views/main.js").unwrap(), vec!["views", "main.js"]);
        // 前导/连续斜杠与 "." 折叠
        assert_eq!(sanitize_segments("/views//main.js").unwrap(), vec!["views", "main.js"]);
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(mime_for_path("views/main.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for_path("assets/main.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for_path("manifest.json"), "application/json; charset=utf-8");
        assert_eq!(mime_for_path("icon.svg"), "image/svg+xml");
        assert_eq!(mime_for_path("font.woff2"), "font/woff2");
        assert_eq!(mime_for_path("noext"), "application/octet-stream");
    }

    #[test]
    fn builtin_dist_serves_example_bundle() {
        // dist 为构建产物且被 gitignore：fresh clone 上缺席，此时跳过
        // （构建：cd code/plugins && npm run build:example）。
        // 编译期候选根恒指向 code/plugins（CARGO_MANIFEST_DIR 语义），
        // spark-example dist 已构建时应能取到 bundle 与 manifest
        let dist_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/spark-example/dist");
        if !dist_root.join("views/main.js").is_file() {
            eprintln!("skip: spark-example dist 未构建（cd code/plugins && npm run build:example）");
            return;
        }
        let data_dir = Path::new("/nonexistent-spark-data-dir");
        let bundle = resolve_plugin_resource(data_dir, "spark-example", "views/main.js");
        assert!(bundle.is_some(), "spark-example dist/views/main.js 应可经内置 dist 解析");
        let manifest = resolve_plugin_resource(data_dir, "spark-example", "manifest.json");
        assert!(manifest.is_some());
    }

    #[test]
    fn traversal_rejected_end_to_end() {
        let data_dir = Path::new("/nonexistent-spark-data-dir");
        assert!(resolve_plugin_resource(data_dir, "spark-example", "../manifest.json").is_none());
        assert!(resolve_plugin_resource(data_dir, "..", "views/main.js").is_none());
        assert!(resolve_plugin_resource(data_dir, "spark-example", "..\\views\\main.js").is_none());
    }

    // ------------------------------------------------------------------
    // 安装包定位（fail-closed：只信 plugin-market-state.json 的 packagePath）
    // ------------------------------------------------------------------

    /// 构造独立临时数据目录（tag 区分用例，避免并行测试互相踩）
    fn temp_data_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spark-plugin-src-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写一个最小 .spkg（JSON 容器：files 精确匹配 + base64 内容）
    fn write_spkg(spkg_path: &Path, entries: &[(&str, &[u8])]) {
        fs::create_dir_all(spkg_path.parent().unwrap()).unwrap();
        let files: Vec<serde_json::Value> = entries
            .iter()
            .map(|(path, bytes)| {
                serde_json::json!({
                    "path": path,
                    "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
                })
            })
            .collect();
        fs::write(spkg_path, serde_json::json!({ "files": files }).to_string()).unwrap();
    }

    /// 写 plugin-market-state.json（单插件记录；enabled 可配）
    fn write_market_state(data_dir: &Path, plugin_id: &str, spkg_path: &Path, enabled: bool) {
        let state = serde_json::json!({
            "installed": {
                plugin_id: {
                    "pluginId": plugin_id,
                    "version": "1.0.0",
                    "packagePath": spkg_path.to_string_lossy(),
                    "sha256": "",
                    "size": 0,
                    "installedAt": 0,
                    "enabled": enabled,
                    "grantedPermissions": []
                }
            }
        });
        fs::write(data_dir.join("plugin-market-state.json"), state.to_string()).unwrap();
    }

    #[test]
    fn installed_spkg_served_via_recorded_package_path() {
        // packagePath 首选定位：状态记录指向哪个包就服务哪个包
        let data_dir = temp_data_dir("pkg-path");
        let spkg = data_dir.join("plugins/demo-plugin/packages/demo-plugin-1.0.0.spkg");
        write_spkg(&spkg, &[("views/main.js", b"hello")]);
        write_market_state(&data_dir, "demo-plugin", &spkg, true);

        let resolved = resolve_plugin_resource(&data_dir, "demo-plugin", "views/main.js");
        assert_eq!(resolved, Some((b"hello".to_vec(), "text/javascript; charset=utf-8")));
    }

    #[test]
    fn unrecorded_spkg_not_served() {
        // 包文件存在但市场状态无记录：fail-closed 拒服（不再扫描 packages 目录猜测）
        let data_dir = temp_data_dir("unrecorded");
        let spkg = data_dir.join("plugins/demo-plugin/packages/demo-plugin-1.0.0.spkg");
        write_spkg(&spkg, &[("views/main.js", b"hello")]);

        assert!(resolve_plugin_resource(&data_dir, "demo-plugin", "views/main.js").is_none());
    }

    #[test]
    fn disabled_plugin_not_served() {
        // enabled = false 整域拒服（停用即关源）
        let data_dir = temp_data_dir("disabled");
        let spkg = data_dir.join("plugins/demo-plugin/packages/demo-plugin-1.0.0.spkg");
        write_spkg(&spkg, &[("views/main.js", b"hello")]);
        write_market_state(&data_dir, "demo-plugin", &spkg, false);

        assert!(resolve_plugin_resource(&data_dir, "demo-plugin", "views/main.js").is_none());
    }

    #[test]
    fn corrupt_market_state_not_served() {
        // 状态文件存在但解析失败：fail-closed 拒服（不降级为目录扫描）
        let data_dir = temp_data_dir("corrupt");
        let spkg = data_dir.join("plugins/demo-plugin/packages/demo-plugin-1.0.0.spkg");
        write_spkg(&spkg, &[("views/main.js", b"hello")]);
        fs::write(data_dir.join("plugin-market-state.json"), b"{ not json").unwrap();

        assert!(resolve_plugin_resource(&data_dir, "demo-plugin", "views/main.js").is_none());
    }

    #[test]
    fn spkg_traversal_entry_not_served() {
        // 固化精确匹配的安全性：容器内含 `../` 条目时，
        // 规范化后的请求路径不会命中它，直接请求 `../` 又被路径校验拦下
        let data_dir = temp_data_dir("traversal-entry");
        let spkg = data_dir.join("plugins/demo-plugin/packages/demo-plugin-1.0.0.spkg");
        write_spkg(&spkg, &[("../evil.js", b"pwned")]);
        write_market_state(&data_dir, "demo-plugin", &spkg, true);

        assert!(resolve_plugin_resource(&data_dir, "demo-plugin", "../evil.js").is_none());
        assert_eq!(read_from_spkg(&spkg, "evil.js"), None);
    }
}
