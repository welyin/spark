//! HTTP 代理配置：解决大陆用户 updater / 市场链路直连 GitHub 失败的问题。
//!
//! 链路：market/sources.rs 的共享 reqwest 客户端（OnceLock，首次使用时定型）
//! 与 tauri-plugin-updater 的 reqwest 客户端都在创建时读取
//! `SPARK_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` 环境变量。因此：
//! - 启动 setup 尽早调用 [`init_proxy_from_disk`] 注入环境变量，保证两类
//!   客户端首次创建时就能吃到代理；
//! - `system_set_proxy` 保存时同步更新环境变量，对**后续新建**的客户端立即
//!   生效；已建立的客户端不追溯，需重启应用（UI 保存时已提示）。
//!
//! 持久化：`<data_dir>/spark-proxy.json`，形如 `{"proxy": "127.0.0.1:29290"}`
//! 或 `{"proxy": null}`；存裸 `host:port`，注入环境变量时补 `http://` scheme
//! （reqwest `Proxy::all` 需要完整 URL）。

use std::path::Path;

/// 代理配置文件名（data_dir 下）。
const PROXY_FILE_NAME: &str = "spark-proxy.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct ProxyFile {
    proxy: Option<String>,
}

/// 入参校验：空串（或全空白）视为关闭 → `Ok(None)`；否则须为 `host:port`
/// （IPv4 / `[IPv6]` / 域名 + 1-65535 端口）。通过时返回规范化后的 `host:port`。
pub(crate) fn validate_proxy(input: &str) -> Result<Option<String>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains("://") || trimmed.contains('/') {
        return Err("代理只需填写主机和端口（如 127.0.0.1:29290），不要包含协议或路径".to_string());
    }
    let (host, port) = trimmed
        .rsplit_once(':')
        .ok_or_else(|| "代理格式应为 主机:端口（如 127.0.0.1:29290）".to_string())?;
    let host = host.trim();
    let port = port.trim();
    let port_num: u32 = port
        .parse()
        .map_err(|_| format!("代理端口无效：{port}（应为 1-65535）"))?;
    if !(1..=65535).contains(&port_num) {
        return Err(format!("代理端口无效：{port}（应为 1-65535）"));
    }
    // 主机：IPv4/IPv6 字面量（IPv6 需 [...] 包裹，与 URL 写法一致）或域名
    // （字母数字、点、连字符）。
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'));
    let host_ok = match bare {
        Some(inner) => inner.parse::<std::net::Ipv6Addr>().is_ok(),
        None => {
            host.parse::<std::net::IpAddr>().is_ok()
                || (!host.is_empty()
                    && host.len() <= 253
                    && host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        }
    };
    if !host_ok {
        return Err(format!("代理主机无效：{host}（应为 IP 或域名）"));
    }
    // 归一化：裸 IPv6 字面量补 [...]（拼进 http:// URL 时方括号必需）
    let normalized_host = match bare {
        Some(_) => host.to_string(),
        None => match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V6(_)) => format!("[{host}]"),
            _ => host.to_string(),
        },
    };
    Ok(Some(format!("{normalized_host}:{port_num}")))
}

/// 读持久化配置；文件缺失或损坏一律按未设置处理（损坏配置不阻断启动/读取）。
pub(crate) fn load_proxy(data_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(data_dir.join(PROXY_FILE_NAME)).ok()?;
    let file: ProxyFile = serde_json::from_str(&content).ok()?;
    file.proxy.filter(|p| !p.trim().is_empty())
}

/// 写持久化配置（None 落 `{"proxy": null}`，保留文件便于排查）。
pub(crate) fn save_proxy(data_dir: &Path, proxy: Option<&str>) -> Result<(), String> {
    let file = ProxyFile {
        proxy: proxy.map(|p| p.to_string()),
    };
    let content = serde_json::to_string_pretty(&file).map_err(|e| format!("代理配置序列化失败：{e}"))?;
    std::fs::write(data_dir.join(PROXY_FILE_NAME), content).map_err(|e| format!("代理配置写入失败：{e}"))
}

/// 注入/清除代理环境变量：Some 时三个变量统一写 `http://host:port`（
/// SPARK_PROXY 优先、HTTPS_PROXY/ALL_PROXY 兜底，对齐 sources.rs 的读取顺序），
/// None 时全部移除（显式关闭）。只影响本进程后续新建的 reqwest 客户端。
pub(crate) fn apply_proxy_env(proxy: Option<&str>) {
    match proxy {
        Some(value) => {
            let url = format!("http://{value}");
            std::env::set_var("SPARK_PROXY", &url);
            std::env::set_var("HTTPS_PROXY", &url);
            std::env::set_var("ALL_PROXY", &url);
        }
        None => {
            std::env::remove_var("SPARK_PROXY");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("ALL_PROXY");
        }
    }
}

/// 启动早期注入：仅当配置了代理时写环境变量；未配置时不动既有环境
/// （用户可能自行设置过 HTTPS_PROXY，启动时不应替其清除）。
pub(crate) fn init_proxy_from_disk(data_dir: &Path) {
    if let Some(proxy) = load_proxy(data_dir) {
        apply_proxy_env(Some(&proxy));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- 格式校验矩阵 ----------------

    #[test]
    fn empty_or_blank_means_disabled() {
        assert_eq!(validate_proxy(""), Ok(None));
        assert_eq!(validate_proxy("   "), Ok(None));
        assert_eq!(validate_proxy(" \t\n "), Ok(None));
    }

    #[test]
    fn valid_ipv4_and_domain_accepted() {
        assert_eq!(validate_proxy("127.0.0.1:29290"), Ok(Some("127.0.0.1:29290".to_string())));
        assert_eq!(validate_proxy("192.168.1.1:8080"), Ok(Some("192.168.1.1:8080".to_string())));
        assert_eq!(validate_proxy("proxy.example.com:443"), Ok(Some("proxy.example.com:443".to_string())));
        // 首尾空白容忍；端口前导零归一化
        assert_eq!(validate_proxy(" 127.0.0.1:08080 "), Ok(Some("127.0.0.1:8080".to_string())));
    }

    #[test]
    fn valid_bracketed_ipv6_accepted() {
        assert_eq!(validate_proxy("[::1]:8080"), Ok(Some("[::1]:8080".to_string())));
    }

    #[test]
    fn port_out_of_range_rejected() {
        assert!(validate_proxy("127.0.0.1:0").is_err());
        assert!(validate_proxy("127.0.0.1:65536").is_err());
        assert!(validate_proxy("127.0.0.1:99999").is_err());
        assert!(validate_proxy("127.0.0.1:abc").is_err());
        assert!(validate_proxy("127.0.0.1:-1").is_err());
        // 边界：1 与 65535 合法
        assert!(validate_proxy("127.0.0.1:1").is_ok());
        assert!(validate_proxy("127.0.0.1:65535").is_ok());
    }

    #[test]
    fn malformed_input_rejected() {
        // 带协议/路径
        assert!(validate_proxy("http://127.0.0.1:8080").is_err());
        assert!(validate_proxy("127.0.0.1:8080/path").is_err());
        // 缺端口 / 缺主机
        assert!(validate_proxy("127.0.0.1").is_err());
        assert!(validate_proxy(":8080").is_err());
        assert!(validate_proxy("127.0.0.1:").is_err());
        // 非法主机字符
        assert!(validate_proxy("bad host:8080").is_err());
        assert!(validate_proxy("主机:8080").is_err());
    }

    #[test]
    fn bare_ipv6_normalized_to_bracketed() {
        // 裸 IPv6（rsplit 后主机段恰好是合法 IPv6 字面量）归一化为 [...] 形式
        assert_eq!(validate_proxy("::1:8080"), Ok(Some("[::1]:8080".to_string())));
    }

    // ---------------- 持久化往返 ----------------

    #[test]
    fn persistence_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 初始无文件 → None
        assert_eq!(load_proxy(dir.path()), None);
        // 写入 → 读回一致
        save_proxy(dir.path(), Some("127.0.0.1:29290")).expect("save");
        assert_eq!(load_proxy(dir.path()), Some("127.0.0.1:29290".to_string()));
        // 文件内容形如 {"proxy": "..."}
        let raw = std::fs::read_to_string(dir.path().join(PROXY_FILE_NAME)).expect("read");
        assert!(raw.contains("\"proxy\": \"127.0.0.1:29290\""), "unexpected: {raw}");
        // 关闭 → {"proxy": null} → 读回 None
        save_proxy(dir.path(), None).expect("save none");
        assert_eq!(load_proxy(dir.path()), None);
        let raw = std::fs::read_to_string(dir.path().join(PROXY_FILE_NAME)).expect("read");
        assert!(raw.contains("\"proxy\": null"), "unexpected: {raw}");
    }

    #[test]
    fn corrupted_file_treated_as_unset() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(PROXY_FILE_NAME), "not json").expect("write");
        assert_eq!(load_proxy(dir.path()), None);
        // proxy 字段为空串同样视为未设置
        std::fs::write(dir.path().join(PROXY_FILE_NAME), "{\"proxy\": \"\"}").expect("write");
        assert_eq!(load_proxy(dir.path()), None);
    }

    // ---------------- 环境变量注入/清除 ----------------

    #[test]
    fn env_set_and_clear() {
        // 环境变量是进程全局状态，set/clear 断言放在同一测试内串行执行，
        // 避免并行用例互相干扰。
        apply_proxy_env(Some("127.0.0.1:29290"));
        assert_eq!(std::env::var("SPARK_PROXY").as_deref(), Ok("http://127.0.0.1:29290"));
        assert_eq!(std::env::var("HTTPS_PROXY").as_deref(), Ok("http://127.0.0.1:29290"));
        assert_eq!(std::env::var("ALL_PROXY").as_deref(), Ok("http://127.0.0.1:29290"));
        apply_proxy_env(None);
        assert!(std::env::var("SPARK_PROXY").is_err());
        assert!(std::env::var("HTTPS_PROXY").is_err());
        assert!(std::env::var("ALL_PROXY").is_err());
    }

    #[test]
    fn init_from_disk_only_applies_when_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        apply_proxy_env(None);
        // 未配置：不动既有环境（先由外部设置一个，确认不被清除）
        std::env::set_var("HTTPS_PROXY", "http://external:1");
        init_proxy_from_disk(dir.path());
        assert_eq!(std::env::var("HTTPS_PROXY").as_deref(), Ok("http://external:1"));
        // 已配置：注入覆盖
        save_proxy(dir.path(), Some("10.0.0.1:8080")).expect("save");
        init_proxy_from_disk(dir.path());
        assert_eq!(std::env::var("HTTPS_PROXY").as_deref(), Ok("http://10.0.0.1:8080"));
        apply_proxy_env(None);
    }
}
