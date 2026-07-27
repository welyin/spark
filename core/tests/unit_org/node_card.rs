use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use libp2p::identity::Keypair;
use serde_json::Value;

use spark_core::org::node_card::*;

fn make_keypair() -> Keypair {
    Keypair::generate_ed25519()
}

fn peer_id_of(keypair: &Keypair) -> String {
    libp2p::PeerId::from_public_key(&keypair.public()).to_base58()
}

fn sample_addrs() -> Vec<String> {
    vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()]
}

#[test]
fn roundtrip_with_recovery_token() {
    let keypair = make_keypair();
    let peer_id = peer_id_of(&keypair);
    let token = "ab".repeat(32);
    let now = 1_720_000_000_000i64;
    let code = make_node_card(
        &keypair,
        &peer_id,
        &sample_addrs(),
        now,
        Some(token.clone()),
    )
    .unwrap();
    let card = parse_and_verify_node_card(&code, now).expect("must verify");
    assert_eq!(card.peer_id, peer_id);
    assert_eq!(card.addresses, sample_addrs());
    assert_eq!(card.timestamp, now);
    assert_eq!(card.recovery_token.as_deref(), Some(token.as_str()));
}

#[test]
fn roundtrip_without_token_drops_key_but_payload_has_null() {
    let keypair = make_keypair();
    let peer_id = peer_id_of(&keypair);
    let now = 1_720_000_000_000i64;
    let code = make_node_card(&keypair, &peer_id, &sample_addrs(), now, None).unwrap();
    // 线上对象缺 recoveryToken 时丢键
    let bytes = URL_SAFE_NO_PAD.decode(code.as_bytes()).unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("recoveryToken").is_none());
    // 载荷里却是 null（?? null 归一）
    assert_eq!(
        build_node_card_payload("12D3KooWTest", &[], 1_720_000_000_000, None),
        "{\"type\":\"spark-node-card\",\"version\":1,\"peerId\":\"12D3KooWTest\",\"addresses\":[],\"timestamp\":1720000000000,\"recoveryToken\":null}"
    );
    let card = parse_and_verify_node_card(&code, now).expect("must verify");
    assert_eq!(card.recovery_token, None);
}

#[test]
fn freshness_window_abs_semantics() {
    let keypair = make_keypair();
    let peer_id = peer_id_of(&keypair);
    let now = 1_720_000_000_000i64;
    // 过期（过去）
    let stale = make_node_card(
        &keypair,
        &peer_id,
        &sample_addrs(),
        now - NODE_CARD_MAX_AGE_MS - 1,
        None,
    )
    .unwrap();
    assert_eq!(
        parse_and_verify_node_card(&stale, now),
        Err(NodeCardReject::Stale)
    );
    // 未来 10 min 内可接受（Math.abs 口径），恰在边界可接受
    let future = make_node_card(
        &keypair,
        &peer_id,
        &sample_addrs(),
        now + NODE_CARD_MAX_AGE_MS,
        None,
    )
    .unwrap();
    assert!(parse_and_verify_node_card(&future, now).is_ok());
    // 超出未来窗口拒绝
    let too_future = make_node_card(
        &keypair,
        &peer_id,
        &sample_addrs(),
        now + NODE_CARD_MAX_AGE_MS + 1,
        None,
    )
    .unwrap();
    assert_eq!(
        parse_and_verify_node_card(&too_future, now),
        Err(NodeCardReject::Stale)
    );
}

#[test]
fn tampered_card_rejected() {
    let keypair = make_keypair();
    let peer_id = peer_id_of(&keypair);
    let now = 1_720_000_000_000i64;
    let card = sign_node_card(
        &keypair,
        &peer_id,
        &sample_addrs(),
        now,
        Some("cd".repeat(32)),
    )
    .unwrap();
    // 篡改地址 → 签名错配
    let mut tampered = card.clone();
    tampered.addresses = vec!["/ip4/9.9.9.9/tcp/15002/ws".to_string()];
    assert_eq!(
        parse_and_verify_node_card(&encode_node_card(&tampered), now),
        Err(NodeCardReject::BadSignature)
    );
    // 篡改 recoveryToken → 签名错配
    let mut tampered_token = card.clone();
    tampered_token.recovery_token = Some("ef".repeat(32));
    assert_eq!(
        parse_and_verify_node_card(&encode_node_card(&tampered_token), now),
        Err(NodeCardReject::BadSignature)
    );
}

#[test]
fn peer_id_mismatch_rejected() {
    let keypair = make_keypair();
    let other = make_keypair();
    let now = 1_720_000_000_000i64;
    // 用 A 的私钥签、peerId 填 B 的 → 验签公钥来自 peerId，必失败
    let card =
        sign_node_card(&keypair, &peer_id_of(&other), &sample_addrs(), now, None).unwrap();
    assert_eq!(
        parse_and_verify_node_card(&encode_node_card(&card), now),
        Err(NodeCardReject::BadSignature)
    );
}

#[test]
fn structure_and_token_shape_rejects() {
    let keypair = make_keypair();
    let peer_id = peer_id_of(&keypair);
    let now = 1_720_000_000_000i64;
    // 垃圾 base64
    assert_eq!(
        parse_and_verify_node_card("!!!not-base64!!!", now),
        Err(NodeCardReject::Malformed)
    );
    // 合法 base64 但非 JSON
    let not_json = URL_SAFE_NO_PAD.encode(b"hello");
    assert_eq!(
        parse_and_verify_node_card(&not_json, now),
        Err(NodeCardReject::Malformed)
    );
    // type 不符（完整形状，仅 type/version 不同 → Structure）
    let mut wrong_type_card =
        sign_node_card(&keypair, &peer_id, &sample_addrs(), now, None).unwrap();
    wrong_type_card.card_type = "other".to_string();
    assert_eq!(
        decode_node_card(&encode_node_card(&wrong_type_card)),
        Err(NodeCardReject::Structure)
    );
    // recoveryToken 形状非法（先签名后换入非法 token 会验签失败；
    // 直接用非法形状 + 合法签名不可能——故用签名跳过路径 verify_node_card 单独测）
    let mut card = sign_node_card(&keypair, &peer_id, &sample_addrs(), now, None).unwrap();
    card.recovery_token = Some("not-hex".to_string());
    assert_eq!(
        verify_node_card(&card, now),
        Err(NodeCardReject::BadRecoveryToken)
    );
    // 形状非法先于验签（即便签名因此失效，reason 也是 bad-recovery-token）
    assert_eq!(
        parse_and_verify_node_card(&encode_node_card(&card), now),
        Err(NodeCardReject::BadRecoveryToken)
    );
}
