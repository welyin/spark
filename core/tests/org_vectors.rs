//! org golden vectors 验收测试：加载 `../spec/vectors/org.json` 逐条断言。
//!
//! 向量由 `desktop/scripts/extract-org-vectors.mts` 用真实 TS 实现生成：
//! - nodeInfoClaim：真实 recover 的 root 密钥签名 → Rust 逐字节重建载荷 +
//!   dalek 验签 nacl 签名 + 同密钥重签字节级一致（Ed25519 确定性签名）
//! - 邀请码：真实 encode/decode 往返（含 24h 边界、未来 createdAt、中文错误消息）
//! - recovery token / stale / buildSnapshot / merge：真实 TS 输出逐字段对齐

use serde_json::Value;
use spark_core::identity::{derive_root_identity, parse_mnemonic};
use spark_core::org::claim::{
    ClaimVerification, NodeInfoClaim, build_node_info_claim_payload, sign_node_info_claim,
    verify_node_info_claim,
};
use spark_core::org::invite::{OrgInviteError, decode_org_invite_at, encode_org_invite};
use spark_core::org::recovery::{active_recovery_tokens, recovery_time_bucket, recovery_token};
use spark_core::org::snapshot::{
    build_organization_sync_snapshot, build_organization_sync_versions,
    is_organization_sync_stale, merge_organization_sync_snapshot,
};
use spark_core::org::tx::OrganizationTransactionRecord;
use spark_core::org::types::{OrganizationRecord, OrganizationSyncVersions};

const NOW: i64 = 1_720_000_000_000;

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/org.json");
    let raw = std::fs::read_to_string(path).expect("read org vectors");
    serde_json::from_str(&raw).expect("parse org vectors")
}

fn versions_from(value: &Value) -> OrganizationSyncVersions {
    serde_json::from_value(value.clone()).expect("sync versions")
}

#[test]
fn node_info_claim_cross_validation() {
    let v = vectors();
    let section = &v["nodeInfoClaim"];
    let mnemonic = section["mnemonic"].as_str().unwrap();
    let parsed = parse_mnemonic(mnemonic).unwrap();
    let identity = derive_root_identity(&parsed.seed);
    assert_eq!(identity.id(), section["rootId"].as_str().unwrap());

    for case in section["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let claim: NodeInfoClaim =
            serde_json::from_value(case["claim"].clone()).expect("claim deserializes");

        // 1. 载荷逐字节一致（固定键序 + peerId ?? null 归一）
        let payload = build_node_info_claim_payload(&claim.unsigned());
        assert_eq!(payload, case["payload"].as_str().unwrap(), "{name}: payload bytes");

        // 2. dalek 验签 TS(nacl) 签名通过；负例 reason 对齐
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::Ok,
            "{name}: verify TS signature"
        );
        let mut tampered = claim.clone();
        tampered.timestamp += 1;
        assert_eq!(
            verify_node_info_claim(&tampered, NOW),
            ClaimVerification::InvalidSignature,
            "{name}: tampered timestamp"
        );
        assert_eq!(
            case["verifyTampered"]["reason"].as_str().unwrap(),
            "invalid-signature"
        );
        assert_eq!(
            verify_node_info_claim(&claim, NOW + 10 * 60 * 1000 + 1),
            ClaimVerification::StaleClaim,
            "{name}: stale"
        );
        assert_eq!(
            case["verifyStale"]["reason"].as_str().unwrap(),
            "stale-claim"
        );

        // 3. 同密钥同载荷重签：Ed25519 确定性签名 ⇒ 与 TS 签名逐字节一致
        let resigned = sign_node_info_claim(
            &identity.signing_key,
            claim.node_info.clone(),
            claim.timestamp,
        );
        assert_eq!(resigned.signature, claim.signature, "{name}: deterministic re-sign");
        assert_eq!(resigned.public_key, claim.public_key, "{name}: publicKey base64");
        assert_eq!(resigned.root_id, claim.root_id, "{name}: rootId");
    }
}

#[test]
fn invite_cross_validation() {
    let v = vectors();
    let section = &v["invite"];

    for case in section["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let code = case["code"].as_str().unwrap();
        let decoded = decode_org_invite_at(code, NOW)
            .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));

        // 与 TS decode 归一化结果逐字段一致
        let expected = &case["decoded"];
        assert_eq!(decoded.org_id, expected["orgId"].as_str().unwrap(), "{name}: orgId");
        assert_eq!(
            decoded.org_name,
            expected["orgName"].as_str().unwrap(),
            "{name}: orgName"
        );
        assert_eq!(
            decoded.inviter.root_id,
            expected["inviter"]["rootId"].as_str().unwrap(),
            "{name}: inviter.rootId"
        );
        let expected_peer = expected["inviter"]["peerId"].as_str();
        assert_eq!(
            decoded.inviter.peer_id.as_deref(),
            expected_peer,
            "{name}: inviter.peerId"
        );
        let expected_addresses: Vec<String> = expected["inviter"]["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        assert_eq!(decoded.inviter.addresses, expected_addresses, "{name}: addresses");
        assert_eq!(
            decoded.created_at,
            expected["createdAt"].as_i64().unwrap(),
            "{name}: createdAt"
        );

        // 归一化结果重编码 ⇒ 与 TS 编码字节级一致（键序 + base64url 无 padding）。
        // 仅干净输入成立；messy 用例的 code 由未归一化 payload 编码，不重编码。
        if case.get("payload").is_some() {
            assert_eq!(encode_org_invite(&decoded), code, "{name}: re-encode bytes");
        }
    }

    // 错误消息逐字对齐（面向用户的中文文案）
    for error_case in section["errors"].as_array().unwrap() {
        let name = error_case["name"].as_str().unwrap();
        let expected_message = error_case["error"].as_str().unwrap();
        let err: OrgInviteError = match name {
            "expired" => {
                // 与生成侧相同的 payload：createdAt = NOW - 24h - 1ms
                let payload = spark_core::org::invite::OrgInvitePayload::new(
                    "org_0123456789abcdef",
                    "",
                    spark_core::org::invite::OrgInviteInviter {
                        root_id: vectors()["nodeInfoClaim"]["rootId"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                        peer_id: Some("12D3KooWInviterPeer".to_string()),
                        addresses: vec![],
                    },
                    NOW - 24 * 60 * 60 * 1000 - 1,
                );
                decode_org_invite_at(&encode_org_invite(&payload), NOW).unwrap_err()
            }
            "empty" => decode_org_invite_at("", NOW).unwrap_err(),
            "malformed" => decode_org_invite_at("!!!not-base64!!!", NOW).unwrap_err(),
            "wrong-type" => {
                let raw = r#"{"type":"other","version":1,"orgId":"org_x","inviter":{},"createdAt":1}"#;
                use base64::Engine;
                let code = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(raw.as_bytes());
                decode_org_invite_at(&code, NOW).unwrap_err()
            }
            other => panic!("unknown invite error case {other}"),
        };
        assert_eq!(err.to_string(), expected_message, "invite error {name}");
    }
}

#[test]
fn recovery_token_cross_validation() {
    let v = vectors();
    let section = &v["recoveryToken"];
    let org_id = section["orgId"].as_str().unwrap();
    let secret = section["recoverySecret"].as_str().unwrap();
    let now = section["nowMs"].as_i64().unwrap();

    assert_eq!(
        recovery_time_bucket(now),
        section["timeBucket"].as_i64().unwrap()
    );
    assert_eq!(
        recovery_token(org_id, secret, section["timeBucket"].as_i64().unwrap()),
        section["token"].as_str().unwrap()
    );
    let active = active_recovery_tokens(org_id, secret, now);
    let expected: Vec<&str> = section["activeTokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert_eq!(active.as_slice(), expected.as_slice());
}

#[test]
fn sync_stale_cross_validation() {
    let v = vectors();
    for case in v["sync"]["staleCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let local = if case["local"].is_null() {
            None
        } else {
            Some(versions_from(&case["local"]))
        };
        let incoming = versions_from(&case["incoming"]);
        assert_eq!(
            is_organization_sync_stale(local.as_ref(), &incoming),
            case["expected"].as_bool().unwrap(),
            "stale case {name}"
        );
    }
}

#[test]
fn sync_versions_from_record_cross_validation() {
    let v = vectors();
    let record: OrganizationRecord =
        serde_json::from_value(v["sync"]["buildSnapshot"]["record"].clone()).unwrap();
    let versions = build_organization_sync_versions(&record, 1_700_000_000_800);
    assert_eq!(
        serde_json::to_value(versions).unwrap(),
        v["sync"]["versionsFromRecord"]
    );
}

#[test]
fn build_snapshot_cross_validation() {
    let v = vectors();
    let section = &v["sync"]["buildSnapshot"];
    let record: OrganizationRecord = serde_json::from_value(section["record"].clone()).unwrap();
    let transactions: Vec<OrganizationTransactionRecord> =
        serde_json::from_value(section["transactions"].clone()).unwrap();
    let snapshot = build_organization_sync_snapshot(&record, &transactions);
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        section["expected"],
        "buildOrganizationSyncSnapshot 输出必须与 TS 逐字段一致"
    );
}

#[test]
fn merge_snapshot_cross_validation() {
    let v = vectors();
    let section = &v["sync"]["merge"];
    let existing: OrganizationRecord = serde_json::from_value(section["existing"].clone()).unwrap();
    let incoming: OrganizationRecord = serde_json::from_value(section["incoming"].clone()).unwrap();
    let snapshot = build_organization_sync_snapshot(&incoming, &[]);
    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, NOW);
    assert_eq!(
        serde_json::to_value(&merged).unwrap(),
        section["expected"],
        "mergeOrganizationSyncSnapshot 输出必须与 TS 逐字段一致"
    );
}
