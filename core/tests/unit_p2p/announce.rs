//! node-announce 覆盖网控制面通告单测。

use libp2p::identity::Keypair;
use spark_core::p2p::announce::*;
use spark_core::p2p::constants::NODE_ANNOUNCE_MAX_AGE_MS;

fn make_keypair() -> Keypair {
    Keypair::generate_ed25519()
}

#[test]
fn node_presence_record_key_is_sha256_of_prefixed_peer_id() {
    use sha2::Digest as _;
    let peer = "12D3KooWTest";
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"spark:node:");
    hasher.update(peer.as_bytes());
    assert_eq!(
        node_presence_record_key(peer),
        hex::encode(hasher.finalize())
    );
}

#[test]
fn verify_announce_text_accepts_signed_and_rejects_tampered() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let addrs = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];
    let announce = sign_node_announce(&keypair, &peer_id, &addrs, 1_000_000).unwrap();
    let text = announce_to_json(&announce);
    // 过期记录也接受（DHT 场景不做新鲜度判定）
    let verified = verify_announce_text(&text).expect("must verify");
    assert_eq!(verified.peer_id, peer_id);

    // 篡改地址 → 签名错配拒绝
    let mut tampered = announce.clone();
    tampered.addresses = vec!["/ip4/9.9.9.9/tcp/15002/ws".to_string()];
    assert!(verify_announce_text(&announce_to_json(&tampered)).is_none());
    // 换成别人的 peerId → 拒绝
    let other = Keypair::generate_ed25519();
    let mut swapped = announce.clone();
    swapped.peer_id = libp2p::PeerId::from_public_key(&other.public()).to_base58();
    assert!(verify_announce_text(&announce_to_json(&swapped)).is_none());
}

#[test]
fn payload_fixed_key_order() {
    let payload = build_node_announce_payload(
        "12D3KooWTest",
        &["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
        1_720_000_000_000,
    );
    assert_eq!(
        payload,
        "{\"type\":\"spark-node-announce\",\"version\":1,\"peerId\":\"12D3KooWTest\",\"addresses\":[\"/ip4/1.2.3.4/tcp/15002/ws\"],\"timestamp\":1720000000000}"
    );
}

#[test]
fn sign_and_accept_roundtrip() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let addrs = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];
    let announce = sign_node_announce(&keypair, &peer_id, &addrs, 1_000_000).unwrap();
    let text = announce_to_json(&announce);

    let mut validator = NodeAnnounceValidator::new();
    let accepted = validator
        .validate(&text, "12D3KooWSelfSelfSelf", &[], 1_000_000)
        .expect("must accept");
    assert_eq!(accepted.peer_id, peer_id);
    assert_eq!(accepted.addresses, addrs);
}

#[test]
fn peer_id_pubkey_extraction_roundtrip() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public());
    let raw = public_key_from_peer_id_str(&peer_id.to_base58()).expect("extractable");
    let expect = keypair.public().try_into_ed25519().unwrap().to_bytes();
    assert_eq!(raw, expect);
}

#[test]
fn reject_chain() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let addrs = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];
    let now = 1_000_000i64;

    // 过期
    let stale = sign_node_announce(
        &keypair,
        &peer_id,
        &addrs,
        now - NODE_ANNOUNCE_MAX_AGE_MS - 1,
    )
    .unwrap();
    let mut v = NodeAnnounceValidator::new();
    assert_eq!(
        v.validate(&announce_to_json(&stale), "self", &[], now),
        Err(AnnounceReject::Stale)
    );

    // 未来 10 min 内可接受（Math.abs 口径）
    let future =
        sign_node_announce(&keypair, &peer_id, &addrs, now + NODE_ANNOUNCE_MAX_AGE_MS).unwrap();
    assert!(
        v.validate(&announce_to_json(&future), "self", &[], now)
            .is_ok()
    );

    // 本机
    let fresh = sign_node_announce(&keypair, &peer_id, &addrs, now).unwrap();
    let mut v = NodeAnnounceValidator::new();
    assert_eq!(
        v.validate(&announce_to_json(&fresh), &peer_id, &[], now),
        Err(AnnounceReject::SelfPeer)
    );

    // 篡改签名
    let mut tampered = fresh.clone();
    tampered.timestamp = now + 1;
    let mut v = NodeAnnounceValidator::new();
    assert_eq!(
        v.validate(&announce_to_json(&tampered), "self", &[], now),
        Err(AnnounceReject::BadSignature)
    );

    // 限流：同一 peerId 60s 内第二次（无新地址）被拒
    let mut v = NodeAnnounceValidator::new();
    assert!(
        v.validate(&announce_to_json(&fresh), "self", &[], now)
            .is_ok()
    );
    assert_eq!(
        v.validate(&announce_to_json(&fresh), "self", &addrs, now + 10_000),
        Err(AnnounceReject::RateLimited)
    );
    // 携带新地址放宽到 5s
    let new_addrs = vec!["/ip4/10.0.0.9/tcp/15002/ws".to_string()];
    let changed = sign_node_announce(&keypair, &peer_id, &new_addrs, now + 6_000).unwrap();
    assert!(
        v.validate(&announce_to_json(&changed), "self", &addrs, now + 6_000)
            .is_ok()
    );

    // 空地址
    let empty = NodeAnnounce {
        addresses: vec![],
        ..fresh.clone()
    };
    let mut v = NodeAnnounceValidator::new();
    assert_eq!(
        v.validate(&announce_to_json(&empty), "self", &[], now),
        Err(AnnounceReject::AddressLimits)
    );
}
