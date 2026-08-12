//! dm 签名载荷 golden vector 验收测试：加载 `../spec/vectors/dm_envelope.json`
//! 逐条断言（固定测试密钥 → `build_signing_payload` 确定输出 + 签名 + 信封
//! 构造/校验往返；对齐 identity_vectors/org_vectors 的消费方式）。
//!
//! vectors 覆盖六类信封线形（kind 用字面量驱动，不依赖实现常量）：
//!  1. `chat`           —— 既有基线（明文 body）
//!  2. `chat` E2E 加密  —— body 为密文对象 `{encrypted, ciphertext, nonce}`，
//!     签名对密文对象做（确定性、验签字节对齐）。AES-256-GCM 加解密本身由
//!     `dm_e2e` 模块（S2）单测覆盖，本 vector 锁定「线形 + 签名字节」。
//!  3. `feed`           —— `{topic, feedId, payload, replyTo?}` 社交定向投递
//!  4. `feed-blob-req`  —— `{hash, offset}` 跨联系人 blob 分块拉取请求
//!  5. `feed-blob-resp` —— `{hash, offset, data, totalBytes}` 分块应答
//!     （线形对齐 pdsync-attachment-resp）。
//!  6. `chat` E2E + 密钥轮换 —— 携带外层可选字段 `ephPub`（临时 X25519 公钥，
//!     与 pubKey/sig 并列、**参与签名**），签名载荷键序 body/ephPub/from/kind/to/ts
//!     （ephPub 插在 body 之后）。实现 `build_signing_payload` / `build_envelope` /
//!     `verify_envelope` 已扩展 ephPub 参数，本向量直接走实现函数锁定线形 +
//!     签名字节（与 §19.1.1 密钥轮换对齐）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::Digest as _;
use spark_core::identity::verify_ed25519_signature;
use spark_core::kernel::dm_envelope::{
    build_envelope_with_eph, build_signing_payload_with_eph, verify_envelope,
};

fn vectors() -> Vec<serde_json::Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/dm_envelope.json");
    let raw = std::fs::read_to_string(path).expect("read dm_envelope vector");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse dm_envelope vector");
    parsed.as_array().cloned().expect("dm_envelope.json 应为数组")
}

#[test]
fn dm_signing_payload_and_signature_match_vectors() {
    let vs = vectors();
    assert!(!vs.is_empty(), "dm_envelope.json 不应为空数组");
    for v in &vs {
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
        let eph_pub = v.get("ephPub").and_then(serde_json::Value::as_str);

        // pubKey 与 rootId 绑定
        let pub_key = key.verifying_key().to_bytes();
        assert_eq!(
            B64.encode(pub_key),
            v["pubKeyBase64"].as_str().unwrap(),
            "kind={kind} pubKey 精确匹配"
        );
        assert_eq!(
            hex::encode(sha2::Sha256::digest(pub_key)),
            from,
            "kind={kind} rootId = sha256hex(pubKey)"
        );

        // 签名载荷确定输出（实现统一处理无/带 ephPub 的键序）：
        //  - 无 ephPub：body/from/kind/to/ts
        //  - 带 ephPub：body/ephPub/from/kind/to/ts（ephPub 插在 body 之后、参与签名）
        let payload = build_signing_payload_with_eph(kind, from, to, ts, body, eph_pub);
        assert_eq!(payload, v["payload"].as_str().unwrap(), "kind={kind} 签名载荷精确匹配");

        // ed25519 确定性签名
        let sig = key.sign(payload.as_bytes());
        assert_eq!(
            B64.encode(sig.to_bytes()),
            v["sigBase64"].as_str().unwrap(),
            "kind={kind} 签名精确匹配"
        );
        assert!(
            verify_ed25519_signature(
                &payload,
                v["sigBase64"].as_str().unwrap(),
                v["pubKeyBase64"].as_str().unwrap(),
            ),
            "kind={kind} 验签应通过"
        );

        // 信封构造/校验往返（now = ts，窗口内）。对所有向量（含 ephPub）走实现
        // 函数：build_envelope 携带 ephPub 时参与签名；verify_envelope 验签并回传
        // ephPub 供接线层解密。
        let envelope = build_envelope_with_eph(kind, from, to, ts, body.clone(), eph_pub, &key);
        let verified = verify_envelope(&envelope, to, ts).expect("vector 信封应校验通过");
        assert_eq!(verified.kind, kind);
        assert_eq!(verified.from, from);
        assert_eq!(verified.ts, ts);
        assert_eq!(verified.body, *body);
        assert_eq!(verified.eph_pub.as_deref(), eph_pub, "kind={kind} ephPub 回传一致");
    }
}

#[test]
fn vectors_cover_new_feed_and_e2e_kinds() {
    let vs = vectors();
    let kinds: Vec<&str> = vs.iter().map(|v| v["kind"].as_str().unwrap()).collect();
    for expected in ["chat", "feed", "feed-blob-req", "feed-blob-resp"] {
        assert!(kinds.contains(&expected), "vectors 应含 kind={expected}");
    }
    // 至少一个 E2E 加密信封（body 携带 encrypted=true）
    assert!(
        vs.iter().any(|v| v["body"]["encrypted"].as_bool() == Some(true)),
        "应含 E2E 加密信封 vector（body.encrypted=true）"
    );
    // 至少一个携带 ephPub 的轮换信封（外层可选字段，参与签名）
    assert!(
        vs.iter().any(|v| v.get("ephPub").and_then(serde_json::Value::as_str).is_some()),
        "应含携带 ephPub 的轮换信封 vector（密钥轮换，临时公钥参与签名）"
    );
}
