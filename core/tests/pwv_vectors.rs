//! E1 口令校验器 golden vectors 消费测试：spec/vectors/pwv.json
//! 的 9 条断言必须与 core/src/pw 真实实现字节级一致。
//!
//! 规格：`wiki/protocol/p2p/personal-data-sync.md` §13。

use base64::Engine as _;
use serde_json::Value;
use spark_core::epoch::RotationReason;
use spark_core::pw::{
    GateDecision, PasswordAck, PasswordVerifier, apply_value, build_ack, build_value,
    compute_ack_mac, decrypt_with_kverify, derive_kack, derive_kverify, get_applied_vts, get_pwv,
    put_applied_vts, should_gate, verify_ack_mac, verify_value,
};
use spark_core::storage::MemoryStorage;

fn load_vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/pwv.json");
    let raw = std::fs::read_to_string(path).expect("read pwv vectors");
    serde_json::from_str(&raw).expect("parse pwv vectors")
}

fn b64_decode32(s: &str) -> [u8; 32] {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("valid base64");
    bytes.try_into().expect("32 bytes")
}

fn b64_decode16(s: &str) -> [u8; 16] {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("valid base64");
    bytes.try_into().expect("16 bytes")
}

fn b64_decode12(s: &str) -> [u8; 12] {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("valid base64");
    bytes.try_into().expect("12 bytes")
}

#[test]
fn pwv_build_verify_roundtrip() {
    let v = load_vectors();
    let t = &v[0];
    assert_eq!(t["id"], "pwv_build_verify_roundtrip");
    let input = &t["input"];
    let expect = &t["expect"];

    let password = input["password"].as_str().unwrap();
    let wrong = input["wrongPassword"].as_str().unwrap();
    let salt = b64_decode16(input["salt"].as_str().unwrap());
    let nonce = b64_decode12(input["nonce"].as_str().unwrap());
    let changed_at = 1760000000000_u64;
    let changed_by = "12D3KooWPqT2nMDSiXUSx5D7fasaxhxKigVhcqfkKqrLghCq9jxz";

    // Kverify 精确值。
    let kverify = derive_kverify(password, &salt).expect("scrypt");
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(kverify),
        expect["kverify"].as_str().unwrap(),
        "Kverify exact"
    );

    // build_value 精确值。
    let pwv = build_value(password, &salt, &nonce, changed_at, changed_by).expect("build");
    assert_eq!(pwv.ct, expect["ct"].as_str().unwrap(), "ct exact");

    // 正确口令验证通过，错误口令失败。
    assert!(verify_value(&pwv, password), "verify with P2");
    assert!(!verify_value(&pwv, wrong), "verify with wrong password");
}

#[test]
fn pwv_kack_and_ack_mac() {
    let v = load_vectors();
    let t = &v[1];
    assert_eq!(t["id"], "pwv_kack_and_ack_mac");
    let input = &t["input"];
    let expect = &t["expect"];

    let kverify = b64_decode32(input["kverify"].as_str().unwrap());
    let peer = input["peer"].as_str().unwrap();
    let v_ts = input["vTs"].as_u64().unwrap();

    let kack = derive_kack(&kverify);
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(kack),
        expect["kack"].as_str().unwrap(),
        "Kack exact"
    );

    let mac = compute_ack_mac(&kack, peer, v_ts);
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(mac),
        expect["mac"].as_str().unwrap(),
        "ack MAC exact"
    );

    // ack 线形对象序列化。
    let ack = build_ack(&kverify, peer, v_ts);
    assert!(
        verify_ack_mac(&kack, peer, v_ts, &ack.mac),
        "verify ack mac"
    );
}

#[test]
fn pwv_forged_rejected_last_good() {
    let v = load_vectors();
    let t = &v[2];
    assert_eq!(t["id"], "pwv_forged_rejected_last_good");
    let input = &t["input"];
    let expect = &t["expect"];

    let kverify = b64_decode32(input["goodKverify"].as_str().unwrap());
    let forged_ct = input["forgedCt"].as_str().unwrap();
    let nonce = input["nonce"].as_str().unwrap();

    // 用真 Kverify 解不开伪造 ct → 拒用，last-good 保留。
    let forged = PasswordVerifier {
        v: 1,
        kdf: "scrypt".to_string(),
        salt: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(), // 占位，不影响解密
        nonce: nonce.to_string(),
        ct: forged_ct.to_string(),
        changed_at: 1760000000000,
        changed_by: "12D3KooWPqT2nMDSiXUSx5D7fasaxhxKigVhcqfkKqrLghCq9jxz".to_string(),
    };
    assert_eq!(
        decrypt_with_kverify(&kverify, &forged).is_some(),
        expect["decryptOk"].as_bool().unwrap(),
        "forged ct rejected by good kverify"
    );
}

#[test]
fn pwv_watermark_monotonic() {
    let v = load_vectors();
    let t = &v[3];
    assert_eq!(t["id"], "pwv_watermark_monotonic");
    let input = &t["input"];
    let expect = &t["expect"];

    let applied_vts = input["appliedVTs"].as_u64().unwrap();
    let incoming_vts = input["incomingVTs"].as_u64().unwrap();

    let mut storage = MemoryStorage::new();
    put_applied_vts(&mut storage, applied_vts).unwrap();

    let incoming = PasswordVerifier {
        v: 1,
        kdf: "scrypt".to_string(),
        salt: "EBESExQVFhcYGRobHB0eHw==".to_string(),
        nonce: "ICEiIyQlJicoKSor".to_string(),
        ct: "WHr/GBcoqDXku/7/EoA25QG2FXnmA0skGrc=".to_string(),
        changed_at: incoming_vts,
        changed_by: "12D3KooWPqT2nMDSiXUSx5D7fasaxhxKigVhcqfkKqrLghCq9jxz".to_string(),
    };

    let applied = apply_value(&mut storage, "node1", &incoming, 1760000000000_i64).unwrap();
    assert_eq!(
        applied,
        expect["apply"].as_bool().unwrap(),
        "replay ignored"
    );
    assert_eq!(
        get_applied_vts(&storage).unwrap(),
        expect["watermarkStays"].as_u64().unwrap(),
        "watermark stays"
    );
    assert!(
        get_pwv(&storage).unwrap().is_none(),
        "pwv not written on replay"
    );
}

#[test]
fn pwv_self_serde_bytes() {
    let v = load_vectors();
    let t = &v[4];
    assert_eq!(t["id"], "pwv_self_serde_bytes");
    let expect_json = t["expect"]["json"].as_str().unwrap();

    let pwv: PasswordVerifier = serde_json::from_str(expect_json).expect("parse pwv:self");
    assert_eq!(
        pwv.to_json().unwrap(),
        expect_json,
        "pwv:self serialization exact"
    );
}

#[test]
fn pwack_wire_bytes() {
    let v = load_vectors();
    let t = &v[5];
    assert_eq!(t["id"], "pwack_wire_bytes");
    let expect_json = t["expect"]["json"].as_str().unwrap();

    let ack: PasswordAck = serde_json::from_str(expect_json).expect("parse pwack");
    assert_eq!(
        ack.to_json().unwrap(),
        expect_json,
        "pwack serialization exact"
    );
}

#[test]
fn pwv_missing_gate_pass() {
    let v = load_vectors();
    let t = &v[6];
    assert_eq!(t["id"], "pwv_missing_gate_pass");
    let input = &t["input"];
    let expect = &t["expect"];

    assert!(!input["pwvPresent"].as_bool().unwrap());
    assert!(!input["ackPresent"].as_bool().unwrap());

    let storage = MemoryStorage::new();
    let decision = should_gate(&storage, "anyPeer", 1760000000000).expect("should_gate");
    assert_eq!(
        decision,
        GateDecision::Pass,
        "missing pwv -> gate passes (non-regressible)"
    );
    assert!(expect["gatePasses"].as_bool().unwrap());
    assert!(expect["nonRegressible"].as_bool().unwrap());
}

#[test]
fn epoch_state_reason_password_reset() {
    let v = load_vectors();
    let t = &v[7];
    assert_eq!(t["id"], "epoch_state_reason_password_reset");
    let expect_json = t["expect"]["json"].as_str().unwrap();

    let state: spark_core::epoch::EpochState = serde_json::from_str(expect_json).expect("parse");
    assert_eq!(state.reason, RotationReason::PasswordReset);
    assert_eq!(state.reason.as_str(), "password_reset");
    assert_eq!(
        serde_json::to_string(&state.reason).unwrap(),
        "\"password_reset\""
    );
    assert_eq!(
        serde_json::to_string(&state).unwrap(),
        expect_json,
        "serialization exact"
    );
}

#[test]
fn epoch_state_unknown_reason_fallback() {
    let v = load_vectors();
    let t = &v[8];
    assert_eq!(t["id"], "epoch_state_unknown_reason_fallback");
    let input_json = t["expect"]["json"].as_str().unwrap();
    let parses_as_unknown = t["expect"]["parsesAsUnknown"].as_bool().unwrap();

    let state: spark_core::epoch::EpochState = serde_json::from_str(input_json).expect("parse");
    assert_eq!(
        state.reason,
        RotationReason::Unknown,
        "unknown reason fallback"
    );
    assert!(parses_as_unknown, "vector marks as unknown fallback");
}
