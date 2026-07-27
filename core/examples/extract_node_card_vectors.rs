//! 提取 nodeCard golden vectors（wiki/protocol/org.md §17）。
//!
//! 一次性运行：`cargo run --example extract_node_card_vectors`，把输出的
//! `nodeCard` 节并入 `../spec/vectors/org.json`（删除 meta.pendingGroups 中的
//! nodeCard 占位）。消费测试：`tests/org_vectors.rs`。
//!
//! 确定性：libp2p Ed25519 密钥 seed 固定 [8u8; 32]（与 orgAddressRecord 的
//! [7u8; 32] 区分），nowMs 固定 1720000000000，与 org.json 其余节的 NOW 口径
//! 一致；recoveryToken 用 recovery.rs 公式的真实计算结果（固定 orgId/secret/桶）。

use libp2p::identity::Keypair;
use serde_json::json;
use spark_core::org::{
    build_node_card_payload, encode_node_card, parse_and_verify_node_card, recovery_time_bucket,
    recovery_token, sign_node_card,
};

fn main() {
    let seed = [8u8; 32];
    let seed_hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    let secret_key = libp2p::identity::ed25519::SecretKey::try_from_bytes(seed)
        .expect("32-byte seed is a valid ed25519 secret key");
    let keypair = Keypair::from(libp2p::identity::ed25519::Keypair::from(secret_key));
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let now_ms: i64 = 1_720_000_000_000;
    let addresses = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];

    // recoveryToken：真实公式（org.md §10 当前桶）
    let org_id = "org_0123456789abcdef";
    let recovery_secret = "ab".repeat(32);
    let token = recovery_token(org_id, &recovery_secret, recovery_time_bucket(now_ms));

    let cases: Vec<_> = [
        ("with-recovery-token", Some(token)),
        ("without-recovery-token", None),
    ]
    .into_iter()
    .map(|(name, token)| {
        let card = sign_node_card(&keypair, &peer_id, &addresses, now_ms, token).unwrap();
        let code = encode_node_card(&card);
        let payload = build_node_card_payload(
            &card.peer_id,
            &card.addresses,
            card.timestamp,
            card.recovery_token.as_deref(),
        );
        let ok = parse_and_verify_node_card(&code, now_ms).is_ok();

        // 过期（过去 10 min + 1ms）→ stale
        let stale_reason =
            parse_and_verify_node_card(&code, now_ms + spark_core::org::NODE_CARD_MAX_AGE_MS + 1)
                .err()
                .map(|e| e.reason())
                .unwrap_or("");

        // 篡改地址 → invalid-signature
        let mut tampered = card.clone();
        tampered.addresses = vec!["/ip4/9.9.9.9/tcp/15002/ws".to_string()];
        let tampered_reason = parse_and_verify_node_card(&encode_node_card(&tampered), now_ms)
            .err()
            .map(|e| e.reason())
            .unwrap_or("");

        // recoveryToken 形状非法 → bad-recovery-token（先于验签）
        let mut bad_token = card.clone();
        bad_token.recovery_token = Some("not-hex".to_string());
        let bad_token_reason = parse_and_verify_node_card(&encode_node_card(&bad_token), now_ms)
            .err()
            .map(|e| e.reason())
            .unwrap_or("");

        json!({
            "name": name,
            "card": card,
            "code": code,
            "payload": payload,
            "verify": { "ok": ok },
            "verifyStale": { "ok": false, "reason": stale_reason },
            "verifyTampered": { "ok": false, "reason": tampered_reason },
            "verifyBadToken": { "ok": false, "reason": bad_token_reason },
        })
    })
    .collect();

    let section = json!({
        "seedHex": seed_hex,
        "nowMs": now_ms,
        "peerId": peer_id,
        "cases": cases,
    });
    println!("{}", serde_json::to_string_pretty(&section).unwrap());
}
