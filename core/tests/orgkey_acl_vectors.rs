//! orgkey-deliver / acl golden vectors 验收测试：加载
//! `../spec/vectors/orgkey-acl.json` 逐条断言（org-followups-batch1 §4，
//! 对齐 orgsync.json 的做法——线形回归基线而非设计输入）。
//!
//! 断言面：签名载荷固定键序逐字节、Ed25519 确定性签名逐字节、验签通过、
//! AclRecord 线形键序、orgkey-deliver 构造→解析往返（wrappedKey/nonce 含
//! 随机数，不钉密文字节）。

use serde_json::Value;
use spark_core::sync::orgsync::{
    AclRecord, acl_sign, acl_sign_payload, acl_verify, build_orgkey_deliver, deliver_sign_payload,
    ed_pk_to_x25519, ed_sk_to_x25519, parse_orgkey_deliver,
};

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/orgkey-acl.json"
    );
    let raw = std::fs::read_to_string(path).expect("read orgkey-acl vectors");
    serde_json::from_str(&raw).expect("parse orgkey-acl vectors")
}

fn seed_key(v: &Value) -> ed25519_dalek::SigningKey {
    let hex = v["signerSeedHex"].as_str().unwrap();
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    ed25519_dalek::SigningKey::from_bytes(&bytes.try_into().unwrap())
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

/// acl 组（§20.7）：签名载荷逐字节 + 确定性签名 + 验签 + 记录线形键序。
#[test]
fn acl_vector() {
    let v = vectors();
    let section = &v["acl"];
    let input = &section["input"];
    let sk = seed_key(section);
    assert_eq!(
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            sk.verifying_key().to_bytes()
        ),
        section["signerPublicKeyB64"].as_str().unwrap(),
        "种子派生公钥与向量一致"
    );

    // 载荷固定键序逐字节
    let payload = acl_sign_payload(
        input["epoch"].as_u64().unwrap(),
        input["orgId"].as_str().unwrap(),
        input["collection"].as_str().unwrap(),
        &strs(&input["owners"]),
        &strs(&input["readers"]),
        input["resetBy"].as_str(),
        input["updatedAt"].as_i64().unwrap(),
    );
    assert_eq!(
        payload,
        section["payload"].as_str().unwrap(),
        "acl 载荷逐字节"
    );

    // Ed25519 确定性签名 → 与向量逐字节一致
    let sig = acl_sign(&sk, &payload);
    assert_eq!(
        sig,
        section["sig"].as_str().unwrap(),
        "acl 确定性签名逐字节"
    );

    // 记录线形（serde 键序 = 结构体字段序）+ 验签通过
    let record: AclRecord = serde_json::from_value(section["recordJson"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&record).unwrap(),
        section["recordJson"],
        "AclRecord 线形往返"
    );
    let pk = ed25519_dalek::VerifyingKey::from_bytes(&sk.verifying_key().to_bytes()).unwrap();
    assert!(
        acl_verify(
            &record,
            input["orgId"].as_str().unwrap(),
            input["collection"].as_str().unwrap(),
            &pk
        ),
        "acl 验签通过"
    );
    // 篡改 → 验签失败
    let mut tampered = record.clone();
    tampered.epoch = 2;
    assert!(!acl_verify(
        &tampered,
        input["orgId"].as_str().unwrap(),
        input["collection"].as_str().unwrap(),
        &pk
    ));
}

/// orgkey-deliver 组（§20.6）：签名载荷逐字节 + 构造→解析往返 + 验签链。
#[test]
fn orgkey_deliver_vector() {
    let v = vectors();
    let section = &v["orgkeyDeliver"];
    let input = &section["input"];
    let sk = seed_key(section);

    // 载荷固定键序逐字节 + 确定性签名
    let payload = deliver_sign_payload(
        input["collection"].as_str().unwrap(),
        input["epoch"].as_u64().unwrap(),
        input["nonce"].as_str().unwrap(),
        input["orgId"].as_str().unwrap(),
        input["recipientRootId"].as_str().unwrap(),
        input["ts"].as_i64().unwrap(),
        input["wrappedKey"].as_str().unwrap(),
    );
    assert_eq!(
        payload,
        section["payload"].as_str().unwrap(),
        "deliver 载荷逐字节"
    );
    use ed25519_dalek::Signer as _;
    let sig = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        sk.sign(payload.as_bytes()).to_bytes(),
    );
    assert_eq!(
        sig,
        section["sig"].as_str().unwrap(),
        "deliver 确定性签名逐字节"
    );

    // 构造 → 解析往返（nonce/wrappedKey 含随机数：断言形状与字段还原，
    // 不钉密文字节）；验签链：从解析字段重建载荷 = 签名载荷
    let owner_x25519 = ed_sk_to_x25519(&sk.to_bytes());
    let recipient_x25519 = ed_pk_to_x25519(&sk.verifying_key().to_bytes()).unwrap();
    let body = build_orgkey_deliver(
        input["orgId"].as_str().unwrap(),
        "fin:pay",
        "1",
        input["epoch"].as_u64().unwrap(),
        &[9u8; 32],
        input["senderRootId"].as_str().unwrap(),
        input["recipientRootId"].as_str().unwrap(),
        &recipient_x25519,
        &sk,
        &owner_x25519,
        input["ts"].as_i64().unwrap(),
    )
    .expect("build deliver");
    let parsed = parse_orgkey_deliver(&body).expect("parse deliver");
    assert_eq!(parsed.org_id, input["orgId"].as_str().unwrap());
    assert_eq!(parsed.collection, input["collection"].as_str().unwrap());
    assert_eq!(parsed.epoch, 1);
    assert_eq!(
        parsed.sender_root_id,
        input["senderRootId"].as_str().unwrap()
    );
    assert_eq!(
        parsed.recipient_root_id,
        input["recipientRootId"].as_str().unwrap()
    );
    assert_eq!(parsed.ts, input["ts"].as_i64().unwrap());
    assert!(!parsed.wrapped_key.is_empty() && !parsed.nonce.is_empty() && !parsed.sig.is_empty());
    // 验签链：解析字段重建载荷 → 签名者对载荷的签名验过
    let rebuilt = deliver_sign_payload(
        &parsed.collection,
        parsed.epoch,
        &parsed.nonce,
        &parsed.org_id,
        &parsed.recipient_root_id,
        parsed.ts,
        &parsed.wrapped_key,
    );
    let rebuilt_sig = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        sk.sign(rebuilt.as_bytes()).to_bytes(),
    );
    assert_eq!(
        rebuilt_sig, parsed.sig,
        "构造的 sig = 签名者对解析载荷的签名"
    );
}
