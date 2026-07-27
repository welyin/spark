//! 集合同步策略注册表单测。

use super::*;
use crate::storage::MemoryStorage;

fn decl(
    strategy: Option<SyncStrategy>,
    governance: bool,
    evidence: bool,
) -> CollectionSchemaDeclaration {
    CollectionSchemaDeclaration {
        sync_strategy: strategy,
        governance,
        enable_evidence: evidence,
    }
}

#[test]
fn encode_uri_component_charset() {
    // 保留字符原样
    assert_eq!(encode_uri_component("AZaz09-_.!~*'()"), "AZaz09-_.!~*'()");
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
    let got = get_collection_schema(&s, "chat", "messages")
        .unwrap()
        .unwrap();
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
    let policy = resolve_collection_policy(
        &s,
        "chat",
        "messages",
        Some(&CollectionSchemaDeclaration::append_only()),
    )
    .unwrap();
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
    s.put(
        &collection_schema_key("d", "c2"),
        "{\"syncStrategy\":\"merge\"}",
    )
    .unwrap();
    assert_eq!(get_collection_schema(&s, "d", "c2").unwrap(), None);
}
