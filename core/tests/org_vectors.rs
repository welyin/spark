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
use spark_core::org::gateway::org_members_dht_key;
use spark_core::org::invite::{OrgInviteError, decode_org_invite_at, encode_org_invite};
use spark_core::org::recovery::{active_recovery_tokens, recovery_time_bucket, recovery_token};
use spark_core::org::snapshot::{
    build_organization_sync_snapshot, build_organization_sync_versions, is_organization_sync_stale,
    merge_organization_sync_snapshot,
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
        assert_eq!(
            payload,
            case["payload"].as_str().unwrap(),
            "{name}: payload bytes"
        );

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
        assert_eq!(
            resigned.signature, claim.signature,
            "{name}: deterministic re-sign"
        );
        assert_eq!(
            resigned.public_key, claim.public_key,
            "{name}: publicKey base64"
        );
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
        assert_eq!(
            decoded.org_id,
            expected["orgId"].as_str().unwrap(),
            "{name}: orgId"
        );
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
        assert_eq!(
            decoded.inviter.addresses, expected_addresses,
            "{name}: addresses"
        );
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
                let raw =
                    r#"{"type":"other","version":1,"orgId":"org_x","inviter":{},"createdAt":1}"#;
                use base64::Engine;
                let code = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());
                decode_org_invite_at(&code, NOW).unwrap_err()
            }
            other => panic!("unknown invite error case {other}"),
        };
        assert_eq!(err.to_string(), expected_message, "invite error {name}");
    }
}

/// 组织私有 DHT key 派生（orgSecretKey 组，p2p-messages.md §15）：
/// key = sha256hex(orgSecret + ":members")，逐 case 对齐。
#[test]
fn org_secret_key_cross_validation() {
    let v = vectors();
    let section = &v["orgSecretKey"];
    for case in section["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let secret = case["orgSecret"].as_str().unwrap();
        let expected = case["key"].as_str().unwrap();
        assert_eq!(
            org_members_dht_key(secret),
            expected,
            "orgSecretKey case {name}"
        );
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

/// 组织地址记录（orgAddressRecord 组，org.md §16）：向量由
/// `core/examples/extract_org_address_vectors.rs` 提取（固定 seed + nowMs）。
/// 逐 case 断言：载荷逐字节、验签 ok、同 seed 确定性重签等于向量签名、
/// 篡改 → invalid-signature、过期 → ttl-window，reason 与向量登记一致。
#[test]
fn org_address_record_cross_validation() {
    use spark_core::org::{
        OrgAddressRecord, OrgAddressVerification, build_org_address_record_payload,
        sign_org_address_record, verify_org_address_record,
    };

    let v = vectors();
    let section = &v["orgAddressRecord"];
    let seed_hex = section["seedHex"].as_str().unwrap();
    let seed: [u8; 32] = {
        let bytes = (0..32)
            .map(|i| u8::from_str_radix(&seed_hex[2 * i..2 * i + 2], 16).unwrap())
            .collect::<Vec<u8>>();
        bytes.try_into().unwrap()
    };
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let now_ms = section["nowMs"].as_i64().unwrap();

    for case in section["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let record: OrgAddressRecord =
            serde_json::from_value(case["record"].clone()).expect("record deserializes");

        // 1. 载荷逐字节一致（固定键序 + displayName ?? null 归一）
        let payload = build_org_address_record_payload(&record.unsigned());
        assert_eq!(
            payload,
            case["payload"].as_str().unwrap(),
            "{name}: payload bytes"
        );

        // 2. 验签正例
        assert_eq!(
            verify_org_address_record(&record, now_ms),
            OrgAddressVerification::Ok,
            "{name}: verify ok"
        );
        assert!(case["verify"]["ok"].as_bool().unwrap());

        // 3. 同 seed 同字段重签：Ed25519 确定性签名 ⇒ 逐字节一致
        let resigned = sign_org_address_record(
            &signing_key,
            &record.org_id,
            record.display_name.clone(),
            record.gateways.clone(),
            record.seq,
            record.published_at,
            record.ttl,
        );
        assert_eq!(resigned, record, "{name}: deterministic re-sign");

        // 4. 篡改 seq → invalid-signature（reason 对齐向量登记）
        let mut tampered = record.clone();
        tampered.seq += 1;
        assert_eq!(
            verify_org_address_record(&tampered, now_ms),
            OrgAddressVerification::InvalidSignature,
            "{name}: tampered seq"
        );
        assert_eq!(
            case["verifyTampered"]["reason"].as_str().unwrap(),
            "invalid-signature"
        );

        // 5. 越过 publishedAt + ttl → ttl-window（reason 对齐向量登记）
        assert_eq!(
            verify_org_address_record(&record, now_ms + record.ttl + 1),
            OrgAddressVerification::TtlWindow,
            "{name}: expired"
        );
        assert_eq!(
            case["verifyExpired"]["reason"].as_str().unwrap(),
            "ttl-window"
        );
    }
}

/// 节点名片（nodeCard 组，org.md §17）：向量由
/// `core/examples/extract_node_card_vectors.rs` 提取（固定 libp2p seed + nowMs）。
/// 逐 case 断言：载荷逐字节（recoveryToken ?? null 口径）、名片串解码往返、
/// 验签 ok、同 seed 确定性重签等于向量名片、过期 → stale、篡改 →
/// invalid-signature、token 形状非法 → bad-recovery-token，reason 与向量登记一致。
#[test]
fn node_card_cross_validation() {
    use spark_core::org::{
        NodeCard, NodeCardReject, build_node_card_payload, encode_node_card,
        parse_and_verify_node_card, sign_node_card,
    };

    let v = vectors();
    let section = &v["nodeCard"];
    let seed_hex = section["seedHex"].as_str().unwrap();
    let seed: [u8; 32] = {
        let bytes = (0..32)
            .map(|i| u8::from_str_radix(&seed_hex[2 * i..2 * i + 2], 16).unwrap())
            .collect::<Vec<u8>>();
        bytes.try_into().unwrap()
    };
    let secret_key = libp2p::identity::ed25519::SecretKey::try_from_bytes(seed).unwrap();
    let keypair =
        libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(secret_key));
    let now_ms = section["nowMs"].as_i64().unwrap();
    // 向量登记的 peerId 必须与 seed 派生一致
    assert_eq!(
        libp2p::PeerId::from_public_key(&keypair.public()).to_base58(),
        section["peerId"].as_str().unwrap()
    );

    for case in section["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let card: NodeCard =
            serde_json::from_value(case["card"].clone()).expect("card deserializes");
        let code = case["code"].as_str().unwrap();

        // 1. 载荷逐字节一致（固定键序 + recoveryToken ?? null 归一）
        let payload = build_node_card_payload(
            &card.peer_id,
            &card.addresses,
            card.timestamp,
            card.recovery_token.as_deref(),
        );
        assert_eq!(
            payload,
            case["payload"].as_str().unwrap(),
            "{name}: payload bytes"
        );

        // 2. 编码往返：向量名片串解码归一后等于向量名片
        let decoded = parse_and_verify_node_card(code, now_ms).expect("{name}: verify ok");
        assert_eq!(decoded, card, "{name}: decode roundtrip");
        assert!(case["verify"]["ok"].as_bool().unwrap());

        // 3. 同 seed 同字段重签：Ed25519 确定性签名 ⇒ 逐字节一致（含名片串）
        let resigned = sign_node_card(
            &keypair,
            &card.peer_id,
            &card.addresses,
            card.timestamp,
            card.recovery_token.clone(),
        )
        .unwrap();
        assert_eq!(resigned, card, "{name}: deterministic re-sign");
        assert_eq!(
            encode_node_card(&resigned),
            code,
            "{name}: deterministic code"
        );

        // 4. 过期 → stale（reason 对齐向量登记）
        assert_eq!(
            parse_and_verify_node_card(code, now_ms + spark_core::org::NODE_CARD_MAX_AGE_MS + 1),
            Err(NodeCardReject::Stale),
            "{name}: stale"
        );
        assert_eq!(case["verifyStale"]["reason"].as_str().unwrap(), "stale");

        // 5. 篡改地址 → invalid-signature（reason 对齐向量登记）
        let mut tampered = card.clone();
        tampered.addresses = vec!["/ip4/9.9.9.9/tcp/15002/ws".to_string()];
        assert_eq!(
            parse_and_verify_node_card(&encode_node_card(&tampered), now_ms),
            Err(NodeCardReject::BadSignature),
            "{name}: tampered addresses"
        );
        assert_eq!(
            case["verifyTampered"]["reason"].as_str().unwrap(),
            "invalid-signature"
        );

        // 6. recoveryToken 形状非法 → bad-recovery-token（先于验签）
        let mut bad_token = card.clone();
        bad_token.recovery_token = Some("not-hex".to_string());
        assert_eq!(
            parse_and_verify_node_card(&encode_node_card(&bad_token), now_ms),
            Err(NodeCardReject::BadRecoveryToken),
            "{name}: bad token shape"
        );
        assert_eq!(
            case["verifyBadToken"]["reason"].as_str().unwrap(),
            "bad-recovery-token"
        );
    }
}
