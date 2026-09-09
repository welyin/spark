//! 存储键、rootId/文本归一化与组织 id/密钥生成。

use rand::Rng;

use super::super::{OrgError, Result};

/// 组织记录存储键前缀（organization/constants.ts:1）。
pub const ORG_META_PREFIX: &str = "org:meta:";

/// 组织记录存储键：`org:meta:<orgId>`。
pub fn organization_key(org_id: &str) -> String {
    format!("{ORG_META_PREFIX}{org_id}")
}

/// 成员条目存储键前缀（阶段四A P1 分拆）：`org:member:{orgId}:`——归属
/// org:structure@v1 键域，lww-record 逐成员一条；成员移除 = 成员记录墓碑。
pub const ORG_MEMBER_PREFIX: &str = "org:member:";

/// 成员条目存储键：`org:member:{orgId}:{rootId}`。
///
/// 【A16 双写过渡，membership §4.4-4】目标名册键 =
/// `org:member:{orgId}:{org_user_id}`（标识面）。双写期条目键**维持 rootId
/// 形态**：rootId ↔ org_user_id 映射由成员记录内 accessKey 自带（rootPubkey
/// 锚点，仅组织内可见），消费点一律经双键访问器解析
/// （`OrganizationRecord::find_member_any_key` / `RosterMember.orgUserId` /
/// sigset 第 4 步双键回查），条目键本身不换——混跑期旧端只认 rootId 键，
/// 换键即名册分裂。
///
/// 切换窗口与移除 rootId 键的条件（**全部满足才执行，属后续任务**，本批只
/// 立条件不动键）：
/// 1. 存量成员 accessKey 全员补齐——`migrate_access_key_backfill` 上线运行
///    一个版本周期，各域名册 `roster_fully_mapped` 恒真；
/// 2. 双键消费面稳定一个版本周期——全网端版本均含双键解析（签名面/城门/
///    寻址/投影），不再出现只认 rootId 的在役端；
/// 3. 切换动作 = 条目键换 org_user_id + 名册移除 rootId 槽位（协议线形
///    变更，须先改 `code/spec/` 规格与 golden vectors 再动实现）。
pub fn org_member_key(org_id: &str, root_id: &str) -> String {
    format!("{ORG_MEMBER_PREFIX}{org_id}:{root_id}")
}

/// rootId 合法性：`trim().toLowerCase()` 后匹配 `^[0-9a-f]{64}$`。
pub fn is_valid_root_id(root_id: &str) -> bool {
    let normalized = root_id.trim().to_lowercase();
    normalized.len() == 64
        && normalized
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// orgId 双形态（org-genesis §2 识别规则 = 长度判别）：
/// legacy `^org_[0-9a-f]{16}$` / 创世哈希型 `^org_[0-9a-f]{64}$`；
/// 两者之外的 orgId 一律非法。
pub fn is_valid_org_id(org_id: &str) -> bool {
    let Some(hex_part) = org_id.strip_prefix("org_") else {
        return false;
    };
    (hex_part.len() == 16 || hex_part.len() == 64)
        && hex_part
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
