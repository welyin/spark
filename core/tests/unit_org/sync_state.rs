use spark_core::org::sync_state::*;
use spark_core::org::types::OrganizationSyncVersions;

fn versions(v: i64) -> OrganizationSyncVersions {
    OrganizationSyncVersions {
        summary_version: v,
        members_version: v,
        member_details_version: v,
        transactions_version: v,
    }
}

fn state(v: i64, at: i64) -> OrgSyncState {
    OrgSyncState {
        versions: versions(v),
        last_synced_at: at,
    }
}

#[test]
fn key_format() {
    assert_eq!(
        org_sync_state_key("peer1", "org_abc"),
        "p2p:org-sync-state:peer1:org_abc"
    );
}

#[test]
fn canonical_shape_roundtrip() {
    let state = state(100, 5000);
    let json = state.to_json();
    assert_eq!(
        json,
        "{\"versions\":{\"summaryVersion\":100,\"membersVersion\":100,\"memberDetailsVersion\":100,\"transactionsVersion\":100},\"lastSyncedAt\":5000}"
    );
    let parsed = OrgSyncState::from_json(&json).unwrap();
    assert_eq!(parsed, state);
    assert!(OrgSyncState::from_json("{bad json").is_none());
}

#[test]
fn polluted_legacy_shape_is_unwrapped() {
    // TS share 路径写入的污染形状：versions = {versions, sections, lastSyncedAt} 外壳
    let polluted = r#"{"versions":{"versions":{"summaryVersion":100,"membersVersion":100,"memberDetailsVersion":100,"transactionsVersion":99},"sections":["summary","members","member-details","transactions"],"lastSyncedAt":4000},"lastSyncedAt":5000}"#;
    let state = OrgSyncState::from_json(polluted).unwrap();
    assert_eq!(state.versions.summary_version, 100);
    assert_eq!(state.versions.transactions_version, 99);
    assert_eq!(state.last_synced_at, 5000);
    // 重新序列化回规范形状（修复随写传播）
    let canonical = state.to_json();
    assert!(canonical.contains("\"summaryVersion\":100"));
    assert!(!canonical.contains("sections"));
}

#[test]
fn expiry_decision() {
    let now = 100 * 24 * 60 * 60 * 1000i64;
    assert!(!is_org_sync_state_expired(
        now - ORG_SYNC_STATE_MAX_AGE_MS,
        now
    ));
    assert!(is_org_sync_state_expired(
        now - ORG_SYNC_STATE_MAX_AGE_MS - 1,
        now
    ));
    assert!(!is_org_sync_state_expired(now, now));
}
