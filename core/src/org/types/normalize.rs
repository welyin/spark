//! 存储键、rootId/文本/插件域归一化与组织 id/密钥生成。

use rand::Rng;

use super::super::{OrgError, Result};

/// 组织记录存储键前缀（organization/constants.ts:1）。
pub const ORG_META_PREFIX: &str = "org:meta:";

/// 组织记录存储键：`org:meta:<orgId>`。
pub fn organization_key(org_id: &str) -> String {
    format!("{ORG_META_PREFIX}{org_id}")
}

/// rootId 合法性：`trim().toLowerCase()` 后匹配 `^[0-9a-f]{64}$`。
pub fn is_valid_root_id(root_id: &str) -> bool {
    let normalized = root_id.trim().to_lowercase();
    normalized.len() == 64
        && normalized
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// `normalizeRootId`：trim + lowercase + 格式校验。
pub fn normalize_root_id(root_id: &str) -> Result<String> {
    let normalized = root_id.trim().to_lowercase();
    if !is_valid_root_id(&normalized) {
        return Err(OrgError::InvalidMemberRootId);
    }
    Ok(normalized)
}

/// `normalizeText`：trim + 连续空白归一为单空格；空串报错（`{label} is required`）。
pub fn normalize_text(value: &str, label: &str) -> Result<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(OrgError::Required(label.to_string()));
    }
    Ok(normalized)
}

/// `normalizePluginDomain`：trim，须以 `plugin:` 开头且前缀后非空。
pub fn normalize_plugin_domain(value: &str) -> Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(OrgError::Required("Base plugin".to_string()));
    }
    if !normalized.starts_with("plugin:") || normalized.len() <= "plugin:".len() {
        return Err(OrgError::InvalidBasePluginDomain);
    }
    Ok(normalized.to_string())
}

/// 生成组织 id：`org_` + 8 随机字节 hex（16 hex，service.ts:88-90）。
pub fn generate_organization_id() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("org_{}", hex::encode(bytes))
}

/// 生成组织恢复盐：32 随机字节 hex（64 hex，service.ts:124）。
pub fn generate_recovery_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 生成组织私有 DHT 派生密钥 orgSecret：32 随机字节 hex（64 hex，org.md §13）。
pub fn generate_org_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
