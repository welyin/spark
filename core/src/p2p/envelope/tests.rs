//! pubsub 信封（P2PMessageBody）与签名体系单测。

use super::*;
use serde_json::json;

fn test_signer() -> EnvelopeSigner {
    // 固定种子 → 固定公钥与确定性签名（Ed25519 为确定性签名）
    EnvelopeSigner::from_seed([7u8; 32])
}

#[test]
fn spki_der_layout() {
    let raw = [1u8; 32];
    let der = spki_der_from_raw(&raw);
    assert_eq!(der.len(), 44);
    assert_eq!(&der[..12], &ED25519_SPKI_DER_PREFIX);
    assert_eq!(&der[12..], &[1u8; 32]);
    // PEM 形态与 Node `export({type:'spki',format:'pem'})` 一致
    let pem = spki_der_pem(&raw);
    assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
    assert!(pem.ends_with("-----END PUBLIC KEY-----\n"));
    assert!(pem.contains(&B64.encode(der)[..60]));
}

#[test]
fn signer_is_ephemeral_and_deterministic() {
    let a = EnvelopeSigner::generate();
    let b = EnvelopeSigner::generate();
    assert_ne!(a.public_key(), b.public_key());
    // 同种子同公钥
    assert_eq!(test_signer().public_key(), test_signer().public_key());
    // 固定种子的公钥字节级固定
    assert_eq!(
        test_signer().public_key(),
        "MCowBQYDK2VwAyEA6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw="
    );
}

#[test]
fn signing_input_key_order_is_insertion_order() {
    let body = build_update_body(
        "notes",
        "items",
        "doc1",
        json!({"text":"hello"}),
        json!({"vv":{"nodeA":1},"ts":1720000000000i64}),
        None,
    );
    let mut envelope = Envelope::new(body, None, 1_720_000_000_000);
    // 未签名时 evidenceHeadHash 恒存在且为 null
    let unsigned_input = envelope.signing_input();
    let input_str = String::from_utf8(unsigned_input.clone()).unwrap();
    assert_eq!(
        input_str,
        concat!(
            "{\"version\":\"1\",\"type\":\"update\",\"domain\":\"notes\",",
            "\"collection\":\"items\",\"id\":\"doc1\",\"payload\":{\"text\":\"hello\"},",
            "\"meta\":{\"vv\":{\"nodeA\":1},\"ts\":1720000000000},",
            "\"evidenceHeadHash\":null,\"timestamp\":1720000000000}"
        )
    );
    envelope.attach_public_key(&test_signer());
    // pubKey 追加在 timestamp 之后；签名输入含 pubKey、不含 signature
    let with_key = String::from_utf8(envelope.signing_input()).unwrap();
    assert!(with_key.ends_with(&format!(",\"pubKey\":\"{}\"}}", test_signer().public_key())));
}

#[test]
fn sign_and_verify_roundtrip_byte_level() {
    let body = build_delete_body(
        "notes",
        "items",
        "doc1",
        json!({"vv":{"nodeA":2},"ts":1720000000123i64}),
        None,
    );
    let mut envelope = Envelope::new(body, Some("ab".repeat(32)), 1_720_000_000_123);
    envelope.sign(&test_signer());
    let text = envelope.to_compact_json();

    // 字节级锚定：固定 key + 固定信封 → 固定签名输入与固定签名
    let expected_input = format!(
        concat!(
            "{{\"version\":\"1\",\"type\":\"delete\",\"domain\":\"notes\",\"collection\":\"items\",",
            "\"id\":\"doc1\",\"payload\":null,\"meta\":{{\"vv\":{{\"nodeA\":2}},\"ts\":1720000000123}},",
            "\"evidenceHeadHash\":\"{}\",\"timestamp\":1720000000123,\"pubKey\":\"{}\"}}"
        ),
        "ab".repeat(32),
        test_signer().public_key()
    );
    let expected_sig = test_signer().sign_base64(expected_input.as_bytes());
    assert!(text.contains(&format!("\"signature\":\"{expected_sig}\"")));
    // signature 键在最后
    assert!(text.ends_with(&format!("\"signature\":\"{expected_sig}\"}}")));

    let verified = parse_and_verify_envelope(&text).expect("must verify");
    assert!(verified.signed);
    assert!(verified.signature_valid);
    assert_eq!(verified.msg_type, "delete");
}

#[test]
fn verify_accepts_pem_public_key() {
    // TS 侧 pubKey 为 PEM：验签侧必须兼容（同一把临时密钥）
    let body = build_org_body("org-share-ack", json!({"syncId":"abc"}));
    let mut envelope = Envelope::new(body, None, 1_720_000_000_000);
    let signer = test_signer();
    envelope.attach_public_key(&signer);
    let input = envelope.signing_input();
    let sig = signer.sign_base64(&input);
    // 手工换成 PEM 形态重签（签名输入中的 pubKey 也必须是 PEM）
    let raw = [7u8; 32];
    let _ = raw;
    let pem = {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        spki_der_pem(&key.verifying_key().to_bytes())
    };
    let mut map = envelope.as_map().clone();
    map.remove("signature");
    map.insert("pubKey".to_string(), Value::String(pem));
    let input2 = serde_json::to_vec(&Value::Object(map.clone())).unwrap();
    let sig2 = signer.sign_base64(&input2);
    let _ = sig;
    map.insert("signature".to_string(), Value::String(sig2));
    let text = serde_json::to_string(&Value::Object(map)).unwrap();
    let verified = parse_and_verify_envelope(&text).expect("pem pubkey must verify");
    assert!(verified.signature_valid);
}

#[test]
fn tampered_envelope_fails_verification() {
    let body = build_update_body(
        "notes",
        "items",
        "doc1",
        json!({"x":1}),
        json!({"vv":{},"ts":1}),
        None,
    );
    let mut envelope = Envelope::new(body, None, 1_720_000_000_000);
    envelope.sign(&test_signer());
    let mut map = envelope.as_map().clone();
    // 篡改 payload
    map.insert("payload".to_string(), json!({"x":2}));
    let text = serde_json::to_string(&Value::Object(map)).unwrap();
    assert!(matches!(
        parse_and_verify_envelope(&text),
        Err(P2pError::SignatureInvalid)
    ));
}

#[test]
fn unsigned_and_unparseable() {
    let body = build_org_body("org-share", json!({}));
    let envelope = Envelope::new(body, None, 1);
    let text = envelope.to_compact_json();
    let parsed = parse_and_verify_envelope(&text).expect("unsigned org-share parses");
    assert!(!parsed.signed);
    assert!(!parsed.signature_valid);
    assert!(matches!(
        parse_and_verify_envelope("not json"),
        Err(P2pError::Malformed(_))
    ));
}

#[test]
fn mandatory_signature_types() {
    assert!(is_signature_mandatory_type("update"));
    assert!(is_signature_mandatory_type("delete"));
    assert!(is_signature_mandatory_type("history-response"));
    assert!(!is_signature_mandatory_type("org-share"));
    assert!(!is_signature_mandatory_type("org-share-ack"));
    assert!(!is_signature_mandatory_type("custom-plugin-msg"));
}

#[test]
fn verification_preserves_wire_key_order() {
    // 验签输入必须与接收文本键序一致：手工构造"乱序"信封文本，
    // 按其自身键序重算签名，验签仍应通过
    let signer = test_signer();
    let unsigned_text = format!(
        "{{\"timestamp\":1,\"type\":\"update\",\"version\":\"1\",\"pubKey\":\"{}\",\"evidenceHeadHash\":null}}",
        signer.public_key()
    );
    let sig = signer.sign_base64(unsigned_text.as_bytes());
    let signed_text = format!(
        "{{\"timestamp\":1,\"type\":\"update\",\"version\":\"1\",\"pubKey\":\"{}\",\"evidenceHeadHash\":null,\"signature\":\"{sig}\"}}",
        signer.public_key()
    );
    let verified = parse_and_verify_envelope(&signed_text).expect("wire order preserved");
    assert!(verified.signature_valid);
}
