//! 清单/包体来源层：`file://` 与本地路径直读、`http://` 一律拒绝、其余按
//! https 下载（reqwest blocking + native-tls 系统信任库）；附 sha256/size 摘要工具。
//!
//! 抓取加固（plugin-dist §3.2-3）：
//! - 重定向策略：拒绝跳转到非 https（https→http 降级即断），最多 5 跳；
//!   响应 final URL 再校验一次 scheme；
//! - 文本/包体一律有界读取：先查 Content-Length 超限即断，流式读入超过
//!   上限 +1 字节即拒（防无界响应撑爆内存）。

use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use sha2::Digest;

/// 目录链路清单/签名文本读取上限（4 MiB，远大于实际清单体量，仅防无界响应）。
const TEXT_FETCH_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// 共享 blocking Client：显式连接/整体超时——市场源不可达（如直连 GitHub 被重置）
/// 时在数秒内失败走错误路径，不允许无超时无限阻塞调用方。
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            // 重定向加固：任何非 https 跳转目标（https→http 降级）即断，最多 5 跳
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.url().scheme() != "https" {
                    attempt.stop()
                } else if attempt.previous().len() >= 5 {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .unwrap_or_default()
    })
}

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn to_file_url(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

/// TS `normalizeFileUrl`：file:// 原样；/ 开头补 file://；其余（https）原样。
pub(crate) fn normalize_file_url(url: &str) -> String {
    if url.starts_with("file://") {
        url.to_string()
    } else if let Some(path) = url.strip_prefix('/') {
        format!("file:///{path}")
    } else {
        url.to_string()
    }
}

/// TS `fetchTextSmart`：file:// 与 / 读本地文件；http:// 拒绝；其余 https GET。
pub(crate) fn fetch_text_smart(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return fs::read_to_string(path).map_err(|e| format!("{e}"));
    }
    if url.starts_with('/') {
        return fs::read_to_string(url).map_err(|e| format!("{e}"));
    }
    if url.starts_with("http://") {
        return Err("Insecure plugin manifest URL is not allowed".to_string());
    }
    fetch_text_http_optional(url, TEXT_FETCH_MAX_BYTES)?.ok_or_else(|| format!("Request failed: {url}, status=404"))
}

/// final URL 复核：重定向策略之外的第二道防线（最终落点必须仍是 https）。
fn ensure_https_final(url: &str, response: &reqwest::blocking::Response) -> Result<(), String> {
    if response.url().scheme() != "https" {
        return Err(format!("Request failed: {url}: redirected to non-https URL"));
    }
    Ok(())
}

/// 有界读取响应体：Content-Length 超限先断（包体下载前即拒），
/// 流式读入超 max_bytes + 1 字节即拒。
fn read_body_capped(
    response: reqwest::blocking::Response,
    url: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    if let Some(len) = response.content_length() {
        if len > max_bytes {
            return Err(format!("Request failed: {url}: response exceeds {max_bytes} bytes"));
        }
    }
    let mut body = Vec::new();
    response
        .take(max_bytes + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("Request failed: {url}: {e}"))?;
    if body.len() as u64 > max_bytes {
        return Err(format!("Request failed: {url}: response exceeds {max_bytes} bytes"));
    }
    Ok(body)
}

/// https GET 文本（repo.rs 仓库锚定链路用）：404 → Ok(None)，其余 >=400 报错；
/// 流式截断读取（超 max_bytes 即拒，声明文件 64 KiB+1 即拒由此落实）。
pub(crate) fn fetch_text_http_optional(url: &str, max_bytes: u64) -> Result<Option<String>, String> {
    if url.starts_with("http://") {
        return Err("Insecure plugin manifest URL is not allowed".to_string());
    }
    let response = http_client()
        .get(url)
        .send()
        .map_err(|e| format!("Request failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if status.as_u16() >= 400 {
        return Err(format!("Request failed: {url}, status={status}"));
    }
    ensure_https_final(url, &response)?;
    let body = read_body_capped(response, url, max_bytes)?;
    let text = String::from_utf8(body).map_err(|e| format!("Request failed: {url}: {e}"))?;
    Ok(Some(text))
}

/// https GET 字节流（repo.rs 包体下载用）：同文本口径，404 → Ok(None)。
pub(crate) fn fetch_bytes_http_optional(url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
    if url.starts_with("http://") {
        return Err("Insecure plugin package URL is not allowed".to_string());
    }
    let response = http_client()
        .get(url)
        .send()
        .map_err(|e| format!("Download failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if status.as_u16() >= 400 {
        return Err(format!("Download failed: {url}, status={status}"));
    }
    ensure_https_final(url, &response)?;
    read_body_capped(response, url, max_bytes).map(Some)
}

/// TS `downloadFile`：https 下载到目标路径（status >= 400 报错；http:// 拒绝；
/// max_bytes 取清单登记 size——Content-Length 超 size 即断，流式截断兜底）。
pub(crate) fn download_file(url: &str, destination: &Path, max_bytes: u64) -> Result<(), String> {
    if url.starts_with("http://") {
        return Err("Insecure plugin package URL is not allowed".to_string());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
    }
    let response = http_client()
        .get(url)
        .send()
        .map_err(|e| format!("Download failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(format!("Download failed: {url}, status={status}"));
    }
    ensure_https_final(url, &response)?;
    let body = read_body_capped(response, url, max_bytes)?;
    fs::write(destination, body).map_err(|e| format!("{e}"))?;
    Ok(())
}

pub(crate) fn compute_file_sha256(path: &Path) -> Result<String, String> {
    let content = fs::read(path).map_err(|e| format!("{e}"))?;
    Ok(hex::encode(sha2::Sha256::digest(content)))
}

pub(crate) fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path).map(|m| m.len()).map_err(|e| format!("{e}"))
}
