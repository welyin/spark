//! 集合同步策略注册表（desktop/src/main/db/schema.ts）。
//!
//! - `append-only`（默认）：仅追加、不覆盖、不删除；治理类数据强制该策略。
//! - `lww`（显式声明）：最后写入获胜，仅用于可容忍覆盖的普通状态数据。
//!
//! 声明持久化在系统域，一旦声明不可变更；同步消息携带的声明副本仅作瞬时兜底
//! （`sanitize_schema_hint` 合法化后），永不写入注册表。
//!
//! 规格见 core/spec/sync-evidence.md §3。

use serde::{Deserialize, Deserializer, Serialize};

use crate::storage::StorageBackend;

/// 集合同步策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncStrategy {
    /// 仅追加（默认/治理强制）。
    #[serde(rename = "append-only")]
    AppendOnly,
    /// 最后写入获胜。
    #[serde(rename = "lww")]
    Lww,
}

impl SyncStrategy {
    /// TS 字符串形式（`"append-only"` / `"lww"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppendOnly => "append-only",
            Self::Lww => "lww",
        }
    }
}

/// `isSyncStrategy`：字符串是否为合法策略值。
pub fn is_sync_strategy(value: &str) -> bool {
    value == "append-only" || value == "lww"
}

/// 宽容反序列化：非法策略字符串映射为 `None`（对齐 TS `isSyncStrategy` 判定失败的情形）。
fn lenient_sync_strategy<'de, D>(deserializer: D) -> std::result::Result<Option<SyncStrategy>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|s| match s.as_str() {
        "append-only" => Some(SyncStrategy::AppendOnly),
        "lww" => Some(SyncStrategy::Lww),
        _ => None,
    }))
}

/// 集合同步策略声明（SDK 层 syncStrategy 必填）。
///
/// `sync_strategy` 为 `None` 表示声明缺失或非法（对齐 TS 中 `isSyncStrategy` 为 false），
/// 由 `resolve_schema_declaration` / `sanitize_schema_hint` 分别作抛错/丢弃处理。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSchemaDeclaration {
    /// 同步策略（缺失或非法时为 `None`）。
    #[serde(
        rename = "syncStrategy",
        default,
        deserialize_with = "lenient_sync_strategy",
        skip_serializing_if = "Option::is_none"
    )]
    pub sync_strategy: Option<SyncStrategy>,
    /// 治理类数据标记：强制 append-only + 链式存证。
    #[serde(default)]
    pub governance: bool,
    /// 链式存证开关；append-only / 治理类集合强制开启，lww 集合可显式开启。
    #[serde(rename = "enableEvidence", default)]
    pub enable_evidence: bool,
}

impl CollectionSchemaDeclaration {
    /// append-only 声明。
    pub fn append_only() -> Self {
        Self {
            sync_strategy: Some(SyncStrategy::AppendOnly),
            governance: false,
            enable_evidence: false,
        }
    }

    /// lww 声明。
    pub fn lww() -> Self {
        Self {
            sync_strategy: Some(SyncStrategy::Lww),
            governance: false,
            enable_evidence: false,
        }
    }
}

/// 归一化后的集合策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCollectionPolicy {
    /// 同步策略。
    #[serde(rename = "syncStrategy")]
    pub sync_strategy: SyncStrategy,
    /// 治理类标记。
    pub governance: bool,
    /// 链式存证开关。
    #[serde(rename = "enableEvidence")]
    pub enable_evidence: bool,
}

/// 持久化的集合策略记录（字段顺序对齐 TS `JSON.stringify(record)`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSchemaRecord {
    /// 同步策略（读取损坏记录时为 `None`，按未声明处理）。
    #[serde(
        rename = "syncStrategy",
        default,
        deserialize_with = "lenient_sync_strategy",
        skip_serializing_if = "Option::is_none"
    )]
    pub sync_strategy: Option<SyncStrategy>,
    /// 治理类标记。
    #[serde(default)]
    pub governance: bool,
    /// 链式存证开关。
    #[serde(rename = "enableEvidence", default)]
    pub enable_evidence: bool,
    /// 数据域。
    pub domain: String,
    /// 集合名。
    pub collection: String,
    /// 声明时间（ms）。
    #[serde(rename = "declaredAt", default)]
    pub declared_at: i64,
}

impl CollectionSchemaRecord {
    /// 提取归一化策略；`sync_strategy` 损坏时返回 `None`。
    fn policy(&self) -> Option<ResolvedCollectionPolicy> {
        self.sync_strategy.map(|sync_strategy| ResolvedCollectionPolicy {
            sync_strategy,
            governance: self.governance,
            enable_evidence: self.enable_evidence,
        })
    }
}

/// 未声明集合的兜底策略：最安全的 append-only + 存证。
pub const DEFAULT_COLLECTION_POLICY: ResolvedCollectionPolicy = ResolvedCollectionPolicy {
    sync_strategy: SyncStrategy::AppendOnly,
    governance: false,
    enable_evidence: true,
};

/// 集合策略记录的存储键前缀。
pub const COLLECTION_SCHEMA_PREFIX: &str = "doc:system:collection-schema:";

/// schema 模块错误。各消息文本与 TS 抛出的 Error.message 逐字一致。
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// syncStrategy 缺失或非法。
    #[error("Invalid collection schema: syncStrategy must be declared as \"append-only\" or \"lww\"")]
    InvalidSyncStrategy,

    /// 治理类集合声明非 append-only（降级尝试）。
    #[error("Governance collections (votes, members, accounts) must use the append-only sync strategy; downgrade is not allowed")]
    GovernanceDowngrade,

    /// 集合名非法。
    #[error("Invalid collection name \"{0}\": only letters, digits, \"_\" and \"-\" are allowed")]
    InvalidCollectionName(String),

    /// 冲突的重复声明（已声明策略不可变更）。
    #[error("{0}")]
    ConflictingDeclaration(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// schema 模块 Result 别名。
pub type Result<T> = std::result::Result<T, SchemaError>;

/// JS `encodeURIComponent`：保留 `A-Z a-z 0-9 - _ . ! ~ * ' ( )`，
/// 其余按 UTF-8 字节 `%XX`（大写 hex）编码。Rust 字符串无孤立代理项，不会抛错。
pub fn encode_uri_component(s: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
    }
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `collectionSchemaKey`：`doc:system:collection-schema:{encodeURIComponent(domain + "/" + collection)}`。
pub fn collection_schema_key(domain: &str, collection: &str) -> String {
    format!("{COLLECTION_SCHEMA_PREFIX}{}", encode_uri_component(&format!("{domain}/{collection}")))
}

/// 集合名合法性：`^[A-Za-z0-9_-]+$`。
fn is_valid_collection_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// `resolveSchemaDeclaration`：归一化声明并应用强制规则。
/// - syncStrategy 必须是两值之一，否则抛错
/// - 治理类声明非 append-only 视为降级尝试，抛错
/// - append-only 强制开启存证；lww 取声明值（默认 false）
pub fn resolve_schema_declaration(
    declaration: Option<&CollectionSchemaDeclaration>,
) -> Result<ResolvedCollectionPolicy> {
    let sync_strategy = declaration.and_then(|d| d.sync_strategy).ok_or(SchemaError::InvalidSyncStrategy)?;
    let governance = declaration.is_some_and(|d| d.governance);
    if governance && sync_strategy != SyncStrategy::AppendOnly {
        return Err(SchemaError::GovernanceDowngrade);
    }
    let enable_evidence = match sync_strategy {
        SyncStrategy::AppendOnly => true,
        SyncStrategy::Lww => declaration.is_some_and(|d| d.enable_evidence),
    };
    Ok(ResolvedCollectionPolicy {
        sync_strategy,
        governance,
        enable_evidence,
    })
}

/// `getCollectionSchema`：读取已声明策略；未声明或记录损坏时返回 `Ok(None)`。
pub fn get_collection_schema<S: StorageBackend>(
    storage: &S,
    domain: &str,
    collection: &str,
) -> Result<Option<CollectionSchemaRecord>> {
    let Some(raw) = storage.get(&collection_schema_key(domain, collection))? else {
        return Ok(None);
    };
    let Ok(parsed) = serde_json::from_str::<CollectionSchemaRecord>(&raw) else {
        return Ok(None);
    };
    if parsed.sync_strategy.is_none() {
        return Ok(None);
    }
    Ok(Some(CollectionSchemaRecord {
        domain: domain.to_string(),
        collection: collection.to_string(),
        ..parsed
    }))
}

/// `declareCollectionSchema`（幂等）：
/// - 首次声明持久化到系统域（`declared_at` 由调用方注入，对齐 TS `Date.now()`）
/// - 重复声明与既有记录一致则直接返回；冲突声明抛错（一旦声明不可变更）
/// - 仅供本地调用；网络来源的声明副本永不允许写入
pub fn declare_collection_schema<S: StorageBackend>(
    storage: &mut S,
    domain: &str,
    collection: &str,
    declaration: &CollectionSchemaDeclaration,
    declared_at: i64,
) -> Result<CollectionSchemaRecord> {
    if !is_valid_collection_name(collection) {
        return Err(SchemaError::InvalidCollectionName(collection.to_string()));
    }
    let policy = resolve_schema_declaration(Some(declaration))?;
    if let Some(existing) = get_collection_schema(storage, domain, collection)? {
        let existing_policy = existing.policy().ok_or(SchemaError::InvalidSyncStrategy)?;
        if existing_policy == policy {
            return Ok(existing);
        }
        return Err(SchemaError::ConflictingDeclaration(format!(
            "Collection \"{collection}\" in {domain} is already declared with syncStrategy \"{}\" \
             (governance={}, enableEvidence={}) and cannot be re-declared",
            existing_policy.sync_strategy.as_str(),
            existing_policy.governance,
            existing_policy.enable_evidence
        )));
    }
    let record = CollectionSchemaRecord {
        sync_strategy: Some(policy.sync_strategy),
        governance: policy.governance,
        enable_evidence: policy.enable_evidence,
        domain: domain.to_string(),
        collection: collection.to_string(),
        declared_at,
    };
    storage.put(&collection_schema_key(domain, collection), &serde_json::to_string(&record)?)?;
    Ok(record)
}

/// `resolveCollectionPolicy`：持久化声明优先；其次调用方兜底声明；最后退回默认（最安全）。
pub fn resolve_collection_policy<S: StorageBackend>(
    storage: &S,
    domain: &str,
    collection: &str,
    fallback_declaration: Option<&CollectionSchemaDeclaration>,
) -> Result<ResolvedCollectionPolicy> {
    if let Some(record) = get_collection_schema(storage, domain, collection)?
        && let Some(policy) = record.policy()
    {
        return Ok(policy);
    }
    if let Some(fallback) = fallback_declaration {
        return resolve_schema_declaration(Some(fallback));
    }
    Ok(DEFAULT_COLLECTION_POLICY)
}

/// `sanitizeSchemaHint`：清洗同步消息携带的策略声明副本。
///
/// 仅作合法化校验（不合法返回 `None`），不做持久化——集合策略注册表只接受本地声明，
/// 网络来源永不写入，防止远端节点通过伪造声明锁死/降级本地集合策略。
pub fn sanitize_schema_hint(
    hint: Option<&CollectionSchemaDeclaration>,
) -> Option<CollectionSchemaDeclaration> {
    let hint = hint?;
    let sync_strategy = hint.sync_strategy?;
    if hint.governance && sync_strategy != SyncStrategy::AppendOnly {
        return None;
    }
    Some(CollectionSchemaDeclaration {
        sync_strategy: Some(sync_strategy),
        governance: hint.governance,
        enable_evidence: hint.enable_evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn decl(strategy: Option<SyncStrategy>, governance: bool, evidence: bool) -> CollectionSchemaDeclaration {
        CollectionSchemaDeclaration {
            sync_strategy: strategy,
            governance,
            enable_evidence: evidence,
        }
    }

    #[test]
    fn encode_uri_component_charset() {
        // 保留字符原样
        assert_eq!(
            encode_uri_component("AZaz09-_.!~*'()"),
            "AZaz09-_.!~*'()"
        );
        assert_eq!(encode_uri_component("chat/messages"), "chat%2Fmessages");
        assert_eq!(encode_uri_component("a b%c"), "a%20b%25c");
        // 非 ASCII 按 UTF-8 字节编码（大写 hex）
        assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
        assert_eq!(encode_uri_component(""), "");
    }

    #[test]
    fn schema_key_matches_ts() {
        assert_eq!(
            collection_schema_key("chat", "messages"),
            "doc:system:collection-schema:chat%2Fmessages"
        );
    }

    #[test]
    fn collection_name_pattern() {
        assert!(is_valid_collection_name("messages"));
        assert!(is_valid_collection_name("a-b_c09"));
        assert!(!is_valid_collection_name(""));
        assert!(!is_valid_collection_name("a.b"));
        assert!(!is_valid_collection_name("中文"));
    }

    #[test]
    fn declare_get_resolve_flow() {
        let mut s = MemoryStorage::new();
        let d = decl(Some(SyncStrategy::Lww), false, true);
        let rec = declare_collection_schema(&mut s, "chat", "messages", &d, 123).unwrap();
        assert_eq!(rec.sync_strategy, Some(SyncStrategy::Lww));
        assert!(rec.enable_evidence);
        assert_eq!(rec.declared_at, 123);

        // 读取
        let got = get_collection_schema(&s, "chat", "messages").unwrap().unwrap();
        assert_eq!(got, rec);

        // 幂等：同策略重复声明返回既有记录
        let again = declare_collection_schema(&mut s, "chat", "messages", &d, 456).unwrap();
        assert_eq!(again.declared_at, 123);

        // 冲突声明抛错，消息对齐 TS
        let err = declare_collection_schema(
            &mut s,
            "chat",
            "messages",
            &CollectionSchemaDeclaration::append_only(),
            789,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Collection \"messages\" in chat is already declared with syncStrategy \"lww\" \
             (governance=false, enableEvidence=true) and cannot be re-declared"
        );

        // 非法集合名
        let err = declare_collection_schema(&mut s, "chat", "bad.name", &d, 1).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Invalid collection name \"bad.name\": only letters, digits, \"_\" and \"-\" are allowed"
        );

        // resolve：持久化声明优先于兜底
        let policy = resolve_collection_policy(&s, "chat", "messages", Some(&CollectionSchemaDeclaration::append_only())).unwrap();
        assert_eq!(policy.sync_strategy, SyncStrategy::Lww);
        // 未声明集合：兜底声明
        let policy = resolve_collection_policy(&s, "chat", "other", Some(&d)).unwrap();
        assert_eq!(policy.sync_strategy, SyncStrategy::Lww);
        // 未声明无兜底：默认
        let policy = resolve_collection_policy(&s, "chat", "other2", None).unwrap();
        assert_eq!(policy, DEFAULT_COLLECTION_POLICY);
    }

    #[test]
    fn sanitize_hint_rules() {
        assert_eq!(sanitize_schema_hint(None), None);
        // 非法策略
        assert_eq!(sanitize_schema_hint(Some(&decl(None, false, false))), None);
        // governance + 非 append-only
        assert_eq!(
            sanitize_schema_hint(Some(&decl(Some(SyncStrategy::Lww), true, false))),
            None
        );
        // 合法声明原样返回（布尔已归一）
        let ok = sanitize_schema_hint(Some(&decl(Some(SyncStrategy::Lww), false, true))).unwrap();
        assert_eq!(ok.sync_strategy, Some(SyncStrategy::Lww));
        assert!(ok.enable_evidence);
    }

    #[test]
    fn corrupted_record_reads_as_none() {
        let mut s = MemoryStorage::new();
        s.put(&collection_schema_key("d", "c"), "not json").unwrap();
        assert_eq!(get_collection_schema(&s, "d", "c").unwrap(), None);
        s.put(&collection_schema_key("d", "c2"), "{\"syncStrategy\":\"merge\"}")
            .unwrap();
        assert_eq!(get_collection_schema(&s, "d", "c2").unwrap(), None);
    }
}
