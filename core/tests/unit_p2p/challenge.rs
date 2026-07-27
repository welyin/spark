//! 节点挑战-应答（node-challenge）帧与签名验证单测。

use libp2p::identity::Keypair;
use spark_core::p2p::challenge::*;
use spark_core::p2p::constants::CHALLENGE_MAX_AGE_MS;

fn make_keypair() -> Keypair {
    Keypair::generate_ed25519()
}

#[test]
fn nonce_is_32_hex_and_random() {
    let a = generate_nonce();
    let b = generate_nonce();
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn request_roundtrip() {
    let text = build_challenge_request("abcd1234", 1_000_000);
    assert_eq!(
        text,
        "{\"type\":\"node-challenge-request\",\"nonce\":\"abcd1234\",\"timestamp\":1000000}"
    );
    let parsed = parse_challenge_request(&text).expect("parseable");
    assert_eq!(parsed.nonce, "abcd1234");
    assert_eq!(parsed.timestamp, 1_000_000);
}

#[test]
fn request_rejects_bad_shape() {
    assert!(parse_challenge_request("not json").is_none());
    assert!(parse_challenge_request("{\"type\":\"other\"}").is_none());
    assert!(
        parse_challenge_request(
            "{\"type\":\"node-challenge-request\",\"nonce\":\"\",\"timestamp\":1}"
        )
        .is_none()
    );
}

#[test]
fn response_sign_verify_roundtrip() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let nonce = generate_nonce();
    let now = 1_720_000_000_000i64;
    let text = sign_challenge_response(&keypair, &nonce, now).expect("sign ok");
    verify_challenge_response(&text, &peer_id, &nonce, now).expect("must verify");
}

#[test]
fn response_rejects_nonce_mismatch_and_stale() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let now = 1_720_000_000_000i64;
    let text = sign_challenge_response(&keypair, "nonce-a", now).expect("sign ok");
    assert_eq!(
        verify_challenge_response(&text, &peer_id, "nonce-b", now),
        Err(ChallengeReject::NonceMismatch)
    );
    assert_eq!(
        verify_challenge_response(&text, &peer_id, "nonce-a", now + CHALLENGE_MAX_AGE_MS + 1),
        Err(ChallengeReject::Stale)
    );
}

#[test]
fn response_rejects_wrong_signer() {
    // 用 B 的私钥签、拿 A 的 peerId 验 → 签名错配
    let keypair_a = make_keypair();
    let keypair_b = make_keypair();
    let peer_a = libp2p::PeerId::from_public_key(&keypair_a.public()).to_base58();
    let nonce = generate_nonce();
    let now = 1_720_000_000_000i64;
    let text = sign_challenge_response(&keypair_b, &nonce, now).expect("sign ok");
    assert_eq!(
        verify_challenge_response(&text, &peer_a, &nonce, now),
        Err(ChallengeReject::BadSignature)
    );
}

#[test]
fn response_rejects_tampered_timestamp() {
    let keypair = make_keypair();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let nonce = generate_nonce();
    let now = 1_720_000_000_000i64;
    let text = sign_challenge_response(&keypair, &nonce, now).expect("sign ok");
    // 篡改 timestamp（破坏签名输入）但仍在新鲜度窗口内
    let tampered = text.replace(&now.to_string(), &(now + 1).to_string());
    assert_eq!(
        verify_challenge_response(&tampered, &peer_id, &nonce, now + 1),
        Err(ChallengeReject::BadSignature)
    );
}
