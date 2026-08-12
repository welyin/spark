//! M3 epoch golden vectors 消费测试：spec/vectors/epoch.json 的
//! box/unbox 往返、密文值、epoch:state 样例必须与 core/src/epoch 真实实现
//! 字节级一致（规格 wiki/protocol/p2p/personal-data-sync.md）。
//!
//! 交叉验证：peerId 不经向量文件自证，而用 libp2p 从私钥独立推导比对；
//! 加密精确值用 nonce 注入变体（`box_ikey_with_nonce` /
//! `wrap_value_with_nonce`）锁定，随机 nonce 变体另做往返。

use ed25519_dalek::SigningKey;
use serde_json::Value;
use spark_core::epoch::{
    EpochState, RotationReason, box_domain_info, box_ikey, box_ikey_with_nonce, ed_pk_to_x25519,
    ed_sk_to_x25519, is_ikey_ciphertext, unbox_ikey, unwrap_value, wrap_value,
    wrap_value_with_nonce,
};

fn load_vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/epoch.json");
    let raw = std::fs::read_to_string(path).expect("read epoch vectors");
    serde_json::from_str(&raw).expect("parse epoch vectors")
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "hex even length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn hex32(v: &Value, field: &str) -> [u8; 32] {
    hex_decode(v[field].as_str().expect("hex str"))
        .try_into()
        .expect("32 bytes")
}

/// libp2p 独立推导 peerId（与向量里的 peerId 交叉验证）。
fn libp2p_peer_id(sk: &[u8; 32]) -> String {
    let secret =
        libp2p::identity::ed25519::SecretKey::try_from_bytes(*sk).expect("ed25519 sk");
    let keypair = libp2p::identity::ed25519::Keypair::from(secret);
    let public = libp2p::identity::PublicKey::from(keypair.public());
    libp2p::PeerId::from_public_key(&public).to_base58()
}

#[test]
fn box_unbox_roundtrip_and_exact_values() {
    let v = load_vectors();
    let b = &v["boxUnbox"];

    let root_id = b["rootId"].as_str().unwrap();
    let epoch = b["epoch"].as_u64().unwrap();
    let writer_sk = hex32(b, "writerSecretKeyHex");
    let recipient_sk = hex32(b, "recipientSecretKeyHex");
    let ikey = hex32(b, "ikeyHex");
    let writer_peer = b["writerPeer"].as_str().unwrap();
    let recipient_peer = b["recipientPeer"].as_str().unwrap();
    let wrapped_key = b["wrappedKeyBase64"].as_str().unwrap();
    let nonce24_b64 = b["nonce24Base64"].as_str().unwrap();

    // 公钥与 peerId 交叉验证（libp2p 独立推导）。
    let writer_pk = SigningKey::from_bytes(&writer_sk).verifying_key().to_bytes();
    let recipient_pk = SigningKey::from_bytes(&recipient_sk)
        .verifying_key()
        .to_bytes();
    assert_eq!(
        hex::encode(writer_pk),
        b["writerPublicKeyHex"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(recipient_pk),
        b["recipientPublicKeyHex"].as_str().unwrap()
    );
    assert_eq!(libp2p_peer_id(&writer_sk), writer_peer, "writer peerId");
    assert_eq!(
        libp2p_peer_id(&recipient_sk),
        recipient_peer,
        "recipient peerId"
    );

    // 域分隔串字节级精确。
    let domain = box_domain_info(root_id, epoch, writer_peer, recipient_peer);
    assert_eq!(
        hex::encode(&domain),
        b["domainInfoHex"].as_str().unwrap(),
        "domainInfo bytes"
    );
    assert!(domain.starts_with(b"ikey-box\0"), "domain prefix");

    let writer_x_priv = ed_sk_to_x25519(&writer_sk);
    let recipient_x_priv = ed_sk_to_x25519(&recipient_sk);
    let writer_x_pub = ed_pk_to_x25519(&writer_pk).expect("writer pk");
    let recipient_x_pub = ed_pk_to_x25519(&recipient_pk).expect("recipient pk");

    // 固定 nonce 加密精确值。
    let nonce24: [u8; 24] = hex_decode("a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7")
        .try_into()
        .unwrap();
    let (got_wrapped, got_nonce) = box_ikey_with_nonce(
        &ikey,
        &recipient_x_pub,
        &writer_x_priv,
        root_id,
        epoch,
        writer_peer,
        recipient_peer,
        &nonce24,
    )
    .expect("box_ikey_with_nonce");
    assert_eq!(got_wrapped, wrapped_key, "wrappedKey exact");
    assert_eq!(got_nonce, nonce24_b64, "nonce24 exact");

    // unbox 还原。
    let got_key = unbox_ikey(
        wrapped_key,
        nonce24_b64,
        &writer_x_pub,
        &recipient_x_priv,
        root_id,
        epoch,
        writer_peer,
        recipient_peer,
    )
    .expect("unbox_ikey");
    assert_eq!(got_key, ikey, "unbox restores ikey");

    // 随机 nonce 公开 API 往返。
    let (rt_wrapped, rt_nonce) = box_ikey(
        &ikey,
        &recipient_x_pub,
        &writer_x_priv,
        root_id,
        epoch,
        writer_peer,
        recipient_peer,
    )
    .expect("box_ikey");
    assert_eq!(
        unbox_ikey(
            &rt_wrapped,
            &rt_nonce,
            &writer_x_pub,
            &recipient_x_priv,
            root_id,
            epoch,
            writer_peer,
            recipient_peer,
        ),
        Some(ikey),
        "random-nonce roundtrip"
    );

    // 错 recipient 私钥（用 writer 的）→ 失败。
    assert!(
        unbox_ikey(
            wrapped_key,
            nonce24_b64,
            &writer_x_pub,
            &writer_x_priv,
            root_id,
            epoch,
            writer_peer,
            recipient_peer,
        )
        .is_none(),
        "wrong recipient key"
    );
    // 错域（epoch+1）→ 失败。
    assert!(
        unbox_ikey(
            wrapped_key,
            nonce24_b64,
            &writer_x_pub,
            &recipient_x_priv,
            root_id,
            epoch + 1,
            writer_peer,
            recipient_peer,
        )
        .is_none(),
        "wrong epoch domain"
    );
    // 低阶点拒绝（H1a）：全零 Montgomery 点。
    assert!(
        box_ikey_with_nonce(
            &ikey,
            &[0u8; 32],
            &writer_x_priv,
            root_id,
            epoch,
            writer_peer,
            recipient_peer,
            &nonce24,
        )
        .is_none(),
        "low-order recipient point rejected at box"
    );
    assert!(
        unbox_ikey(
            wrapped_key,
            nonce24_b64,
            &[0u8; 32],
            &recipient_x_priv,
            root_id,
            epoch,
            writer_peer,
            recipient_peer,
        )
        .is_none(),
        "low-order writer point rejected at unbox"
    );
}

#[test]
fn value_wrap_exact_and_aad_binding() {
    let v = load_vectors();
    let w = &v["valueWrap"];

    let record_key = w["recordKey"].as_str().unwrap();
    let epoch = w["epoch"].as_u64().unwrap();
    let ikey = hex32(w, "ikeyHex");
    let plaintext = w["plaintext"].as_str().unwrap();
    let ciphertext = &w["ciphertext"];

    // 判别规则。
    assert!(is_ikey_ciphertext(ciphertext), "ciphertext detected");
    assert!(!is_ikey_ciphertext(&serde_json::json!({"a": 1})), "plain value");
    assert!(
        !is_ikey_ciphertext(&serde_json::json!({"$enc": "ikey"})),
        "missing fields"
    );
    assert!(
        !is_ikey_ciphertext(&serde_json::json!({
            "$enc": "ikey", "epoch": "2", "nonce": "x", "ct": "y"
        })),
        "epoch must be number"
    );

    // 固定 nonce 加密精确值（含序列化字节序）。
    let nonce12: [u8; 12] = hex_decode("c0c1c2c3c4c5c6c7c8c9cacb").try_into().unwrap();
    let got = wrap_value_with_nonce(&ikey, record_key, epoch, &nonce12, plaintext)
        .expect("wrap_value_with_nonce");
    assert_eq!(&got, ciphertext, "ciphertext value exact");
    assert_eq!(
        serde_json::to_string(&got).unwrap(),
        serde_json::to_string(ciphertext).unwrap(),
        "ciphertext serialization exact"
    );

    // unwrap 还原。
    assert_eq!(
        unwrap_value(&ikey, record_key, ciphertext).as_deref(),
        Some(plaintext),
        "unwrap restores plaintext"
    );

    // 随机 nonce 公开 API 往返。
    let rt = wrap_value(&ikey, record_key, epoch, plaintext).expect("wrap_value");
    assert_eq!(
        unwrap_value(&ikey, record_key, &rt).as_deref(),
        Some(plaintext),
        "random-nonce roundtrip"
    );

    // 错 AAD（记录键不同）→ 失败；错密钥 → 失败；非密文 → None。
    assert!(
        unwrap_value(&ikey, "ct:friend:wrong", ciphertext).is_none(),
        "wrong AAD"
    );
    let mut wrong_key = ikey;
    wrong_key[0] ^= 1;
    assert!(
        unwrap_value(&wrong_key, record_key, ciphertext).is_none(),
        "wrong key"
    );
    assert!(
        unwrap_value(&ikey, record_key, &serde_json::json!({"a": 1})).is_none(),
        "non-ciphertext"
    );
}

#[test]
fn epoch_state_wire_format() {
    let v = load_vectors();
    let state_json = v["epochState"]["json"].as_str().unwrap();
    let b = &v["boxUnbox"];

    let state: EpochState = serde_json::from_str(state_json).expect("parse epoch:state");
    assert_eq!(state.current, 2);
    assert_eq!(state.rotated_at, 1_755_000_000_000);
    assert_eq!(state.rotated_by, b["writerPeer"].as_str().unwrap());
    assert_eq!(state.reason, RotationReason::Revoke);
    assert_eq!(state.reason.as_str(), "revoke");
    assert_eq!(RotationReason::Init.as_str(), "init");
    assert_eq!(RotationReason::PasswordChange.as_str(), "password_change");

    // 序列化字节级（字段序 + camelCase + reason 枚举值）。
    assert_eq!(
        serde_json::to_string(&state).unwrap(),
        state_json,
        "epoch:state serialization exact"
    );

    // 未知 reason 兜底样例。
    let unknown_json = v["epochStateUnknown"]["json"].as_str().unwrap();
    let unknown_state: spark_core::epoch::EpochState =
        serde_json::from_str(unknown_json).expect("unknown reason deserializes");
    assert_eq!(unknown_state.reason, RotationReason::Unknown);

    // reason 全枚举线形值。
    assert_eq!(
        serde_json::to_string(&RotationReason::Init).unwrap(),
        "\"init\""
    );
    assert_eq!(
        serde_json::to_string(&RotationReason::PasswordChange).unwrap(),
        "\"password_change\""
    );
    assert_eq!(
        serde_json::to_string(&RotationReason::PasswordReset).unwrap(),
        "\"password_reset\""
    );
    assert_eq!(serde_json::to_string(&RotationReason::Heal).unwrap(), "\"heal\"");
    assert_eq!(serde_json::to_string(&RotationReason::Unknown).unwrap(), "\"unknown\"");
}
