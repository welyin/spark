//! dm 签名载荷 golden vector 验收测试：加载 `../spec/vectors/dm_envelope.json`
//! 逐条断言（固定测试密钥 → `build_signing_payload` 确定输出 + 签名 + 信封
//! 构造/校验往返；对齐 identity_vectors/org_vectors 的消费方式）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::Digest as _;
use spark_core::identity::verify_ed25519_signature;
use spark_core::kernel::dm_envelope::{build_envelope, build_signing_payload, verify_envelope};

fn vector() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/dm_envelope.json");
    let raw = std::fs::read_to_string(path).expect("read dm_envelope vector");
    serde_json::from_str(&raw).expect("parse dm_envelope vector")
}

#[test]
fn dm_signing_payload_and_signature_match_vector() {
    let v = vector();
    let secret: [u8; 32] = hex::decode(v["secretKeyHex"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let key = SigningKey::from_bytes(&secret);
    let from = v["rootId"].as_str().unwrap();
    let to = v["to"].as_str().unwrap();
    let ts = v["ts"].as_i64().unwrap();
    let body = &v["body"];
    let kind = v["kind"].as_str().unwrap();

    // pubKey 与 rootId 绑定
    let pub_key = key.verifying_key().to_bytes();
    assert_eq!(
        B64.encode(pub_key),
        v["pubKeyBase64"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(sha2::Sha256::digest(pub_key)),
        from,
        "rootId = sha256hex(pubKey)"
    );

    // 签名载荷确定输出
    let payload = build_signing_payload(kind, from, to, ts, body);
    assert_eq!(payload, v["payload"].as_str().unwrap(), "签名载荷精确匹配");

    // ed25519 确定性签名
    let sig = key.sign(payload.as_bytes());
    assert_eq!(
        B64.encode(sig.to_bytes()),
        v["sigBase64"].as_str().unwrap(),
        "签名精确匹配"
    );
    assert!(verify_ed25519_signature(
        &payload,
        v["sigBase64"].as_str().unwrap(),
        v["pubKeyBase64"].as_str().unwrap(),
    ));

    // 信封构造/校验往返（now = ts，窗口内）
    let envelope = build_envelope(kind, from, to, ts, body.clone(), &key);
    let verified = verify_envelope(&envelope, to, ts).expect("vector 信封应校验通过");
    assert_eq!(verified.kind, kind);
    assert_eq!(verified.from, from);
    assert_eq!(verified.ts, ts);
    assert_eq!(verified.body, *body);
}
