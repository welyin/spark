//! M3 epoch golden vectors 生成器：`cargo run -p spark-core --example gen_epoch_vectors`
//! 输出 `code/spec/vectors/epoch.json`（消费测试 `tests/epoch_vectors.rs`）。
//! 所有随机值硬编码为常量，重复运行字节级一致；生成过程含自检断言。

use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use spark_core::epoch::{
    EpochState, RotationReason, box_domain_info, box_ikey_with_nonce, ed_pk_to_x25519,
    ed_sk_to_x25519, unbox_ikey, unwrap_value, wrap_value_with_nonce,
};

/// libp2p 真实口径推导 peerId（Ed25519，identity multihash）。
fn libp2p_peer_id(sk: &[u8; 32]) -> String {
    let secret = libp2p::identity::ed25519::SecretKey::try_from_bytes(*sk).expect("ed25519 sk");
    let keypair = libp2p::identity::ed25519::Keypair::from(secret);
    let public = libp2p::identity::PublicKey::from(keypair.public());
    libp2p::PeerId::from_public_key(&public).to_base58()
}

fn main() {
    let writer_sk = [0x11u8; 32];
    let recipient_sk = [0x22u8; 32];
    let writer_pk = SigningKey::from_bytes(&writer_sk).verifying_key().to_bytes();
    let recipient_pk = SigningKey::from_bytes(&recipient_sk)
        .verifying_key()
        .to_bytes();
    let writer_peer = libp2p_peer_id(&writer_sk);
    let recipient_peer = libp2p_peer_id(&recipient_sk);
    let root_id = hex::encode(Sha256::digest(b"spark-m3-golden-root"));
    let epoch: u64 = 2;
    let mut ikey = [0u8; 32];
    for (i, b) in ikey.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut nonce24 = [0u8; 24];
    for (i, b) in nonce24.iter_mut().enumerate() {
        *b = 0xa0 + i as u8;
    }
    let mut nonce12 = [0u8; 12];
    for (i, b) in nonce12.iter_mut().enumerate() {
        *b = 0xc0 + i as u8;
    }
    let record_key = format!("ct:friend:{root_id}");
    let plaintext = "{\"nickname\":\"Alice\",\"tags\":[\"family\"]}";

    let writer_x_priv = ed_sk_to_x25519(&writer_sk);
    let recipient_x_priv = ed_sk_to_x25519(&recipient_sk);
    let writer_x_pub = ed_pk_to_x25519(&writer_pk).expect("writer pk decompress");
    let recipient_x_pub = ed_pk_to_x25519(&recipient_pk).expect("recipient pk decompress");

    let domain_info = box_domain_info(&root_id, epoch, &writer_peer, &recipient_peer);
    let (wrapped_key, nonce24_b64) = box_ikey_with_nonce(
        &ikey,
        &recipient_x_pub,
        &writer_x_priv,
        &root_id,
        epoch,
        &writer_peer,
        &recipient_peer,
        &nonce24,
    )
    .expect("box_ikey_with_nonce");

    // 自检：unbox 还原；错钥/错域失败；低阶点拒绝。
    assert_eq!(
        unbox_ikey(
            &wrapped_key,
            &nonce24_b64,
            &writer_x_pub,
            &recipient_x_priv,
            &root_id,
            epoch,
            &writer_peer,
            &recipient_peer,
        ),
        Some(ikey),
        "unbox roundtrip"
    );
    assert!(
        unbox_ikey(
            &wrapped_key,
            &nonce24_b64,
            &writer_x_pub,
            &writer_x_priv,
            &root_id,
            epoch,
            &writer_peer,
            &recipient_peer,
        )
        .is_none(),
        "wrong recipient key must fail"
    );
    assert!(
        unbox_ikey(
            &wrapped_key,
            &nonce24_b64,
            &writer_x_pub,
            &recipient_x_priv,
            &root_id,
            3,
            &writer_peer,
            &recipient_peer,
        )
        .is_none(),
        "wrong epoch domain must fail"
    );
    assert!(
        box_ikey_with_nonce(
            &ikey,
            &[0u8; 32],
            &writer_x_priv,
            &root_id,
            epoch,
            &writer_peer,
            &recipient_peer,
            &nonce24,
        )
        .is_none(),
        "low-order recipient point must be rejected"
    );

    let ciphertext = wrap_value_with_nonce(&ikey, &record_key, epoch, &nonce12, plaintext)
        .expect("wrap_value_with_nonce");
    assert_eq!(
        unwrap_value(&ikey, &record_key, &ciphertext).as_deref(),
        Some(plaintext),
        "unwrap roundtrip"
    );
    assert!(
        unwrap_value(&ikey, "ct:friend:wrong", &ciphertext).is_none(),
        "wrong AAD must fail"
    );

    let state = EpochState {
        current: epoch,
        rotated_at: 1_755_000_000_000,
        rotated_by: writer_peer.clone(),
        reason: RotationReason::Revoke,
    };
    let state_json = serde_json::to_string(&state).expect("state serialize");

    let state_unknown = EpochState {
        current: epoch,
        rotated_at: 1_755_000_000_000,
        rotated_by: writer_peer.clone(),
        reason: RotationReason::Unknown,
    };
    let state_unknown_json = serde_json::to_string(&state_unknown).expect("state unknown serialize");
    assert!(
        serde_json::from_str::<EpochState>("{\"current\":2,\"rotatedAt\":1755000000000,\"rotatedBy\":\"12D3KooWPqT2nMDSiXUSx5D7fasaxhxKigVhcqfkKqrLghCq9jxz\",\"reason\":\"heal\"}").is_ok(),
        "new reason 'heal' deserializes"
    );

    let doc = json!({
        "meta": {
            "title": "M3 epoch 选择性密钥轮换 golden vectors（wiki/protocol/p2p/personal-data-sync.md）",
            "generatedBy": "core/examples/gen_epoch_vectors.rs（cargo run -p spark-core --example gen_epoch_vectors）",
            "deterministic": "所有随机值硬编码为常量；重复运行字节级一致",
        },
        "constants": {
            "domainRule": "domainInfo = \"ikey-box\" 0x00 rootId 0x00 epoch(十进制ASCII) 0x00 writerPeer 0x00 recipientPeer",
            "boxRule": "shared=X25519(writerPrivX, recipientPubX)；全零拒绝；boxKey=sha256(shared||domainInfo)；wrappedKey=AES-256-GCM(boxKey, nonce24[0..12], ikey)",
            "valueRule": "ct=AES-256-GCM(ikey, nonce12, plaintext, aad=记录完整键UTF-8)；线形 {$enc:\"ikey\",epoch,nonce,ct}",
            "edToX25519": "TweetNaCl 口径：pub=Edwards→Montgomery u；priv=clamp(sha512(seed)[0..32])",
        },
        "boxUnbox": {
            "description": "ikey 设备 DH 包裹往返（规格 §5.1）",
            "rootId": root_id,
            "epoch": epoch,
            "writerSecretKeyHex": hex::encode(writer_sk),
            "writerPublicKeyHex": hex::encode(writer_pk),
            "writerPeer": writer_peer,
            "recipientSecretKeyHex": hex::encode(recipient_sk),
            "recipientPublicKeyHex": hex::encode(recipient_pk),
            "recipientPeer": recipient_peer,
            "ikeyHex": hex::encode(ikey),
            "domainInfoHex": hex::encode(&domain_info),
            "nonce24Base64": nonce24_b64,
            "wrappedKeyBase64": wrapped_key,
        },
        "valueWrap": {
            "description": "密文值线形（规格 §3）；AAD=记录完整键",
            "recordKey": record_key,
            "epoch": epoch,
            "ikeyHex": hex::encode(ikey),
            "plaintext": plaintext,
            "nonceBase64": base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                nonce12,
            ),
            "ciphertext": ciphertext,
        },
        "epochState": {
            "description": "epoch:state serde 线形（规格 §4，字段序/camelCase；reason 枚举 = init|revoke|password_change|password_reset|heal|unknown，未知值走 Unknown 兜底）",
            "json": state_json,
        },
        "epochStateUnknown": {
            "description": "未知 reason 兜底样例：反序列化 'unknown_reason' 得到 RotationReason::Unknown",
            "json": state_unknown_json,
        },
    });

    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/epoch.json");
    std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()))
        .expect("write vectors");
    println!("written: {out}");
}
