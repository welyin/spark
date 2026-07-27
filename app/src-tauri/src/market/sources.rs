//! 清单/包体来源层：`file://` 与本地路径直读、`http://` 一律拒绝、其余按
//! https 下载（reqwest blocking + native-tls 系统信任库）；附 sha256/size 摘要工具。

use std::fs;
use std::path::Path;

use sha2::Digest;

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
    let response = reqwest::blocking::get(url).map_err(|e| format!("Request failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(format!("Request failed: {url}, status={status}"));
    }
    response
        .text()
        .map_err(|e| format!("Request failed: {url}: {e}"))
}

/// TS `downloadFile`：https 下载到目标路径（status >= 400 报错）。
pub(crate) fn download_file(url: &str, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
    }
    let mut response = reqwest::blocking::get(url).map_err(|e| format!("Download failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(format!("Download failed: {url}, status={status}"));
    }
    let mut file = fs::File::create(destination).map_err(|e| format!("{e}"))?;
    response
        .copy_to(&mut file)
        .map_err(|e| format!("Download failed: {url}: {e}"))?;
    Ok(())
}

pub(crate) fn compute_file_sha256(path: &Path) -> Result<String, String> {
    let content = fs::read(path).map_err(|e| format!("{e}"))?;
    Ok(hex::encode(sha2::Sha256::digest(content)))
}

pub(crate) fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path).map(|m| m.len()).map_err(|e| format!("{e}"))
}
