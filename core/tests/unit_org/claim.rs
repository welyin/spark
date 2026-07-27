//! nodeInfoClaim 签名/验签单测（固定键序载荷、时窗、防重放）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use spark_core::identity::{derive_root_identity, parse_mnemonic};
use spark_core::org::claim::*;
use spark_core::org::types::OrganizationNodeInfo;

const NOW: i64 = 1_720_000_000_000;
const MNEMONIC: &str = "与 祝 产 鸡 永 烂 施 师 蓝 荷 有 邓 朗 防 管 李 原 芳 饿 万 措 走 腰 旅";

fn test_identity() -> spark_core::identity::Identity {
    let parsed = parse_mnemonic(MNEMONIC).unwrap();
    derive_root_identity(&parsed.seed)
}

fn sample_claim(with_peer: bool) -> NodeInfoClaim {
    let identity = test_identity();
    sign_node_info_claim(
        &identity.signing_key,
        OrganizationNodeInfo {
            peer_id: with_peer.then(|| "12D3KooWSelfPeer".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
        },
        NOW,
    )
}

#[test]
fn payload_fixed_key_order_and_null_peer() {
    let claim = sample_claim(false);
    let payload = build_node_info_claim_payload(&claim.unsigned());
    let expected = format!(
        "{{\"type\":\"spark-node-info-claim\",\"version\":1,\"rootId\":\"{}\",\"publicKey\":\"{}\",\"nodeInfo\":{{\"peerId\":null,\"addresses\":[\"/ip4/1.2.3.4/tcp/15002/ws\"]}},\"timestamp\":{}}}",
        claim.root_id, claim.public_key, NOW
    );
    assert_eq!(
        payload, expected,
        "peerId 缺省时载荷中必须为 null（?? null 归一）"
    );
    // 线上 claim 对象本身缺 peerId 时丢键（与载荷序列化不同）
    let wire = serde_json::to_string(&claim).unwrap();
    assert!(
        !wire.contains("peerId"),
        "线上 claim 缺 peerId 应丢键: {wire}"
    );
}

#[test]
fn payload_with_peer_id() {
    let claim = sample_claim(true);
    let payload = build_node_info_claim_payload(&claim.unsigned());
    assert!(payload.contains("\"peerId\":\"12D3KooWSelfPeer\""));
    assert!(!payload.contains("null"));
}

#[test]
fn sign_verify_roundtrip() {
    for with_peer in [true, false] {
        let claim = sample_claim(with_peer);
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::Ok,
            "with_peer={with_peer}"
        );
        // ±10 min 边界内
        assert!(verify_node_info_claim(&claim, NOW + NODE_INFO_CLAIM_MAX_AGE_MS).is_ok());
        assert!(verify_node_info_claim(&claim, NOW - NODE_INFO_CLAIM_MAX_AGE_MS).is_ok());
    }
}

#[test]
fn verify_rejects_stale() {
    let claim = sample_claim(true);
    assert_eq!(
        verify_node_info_claim(&claim, NOW + NODE_INFO_CLAIM_MAX_AGE_MS + 1),
        ClaimVerification::StaleClaim
    );
    assert_eq!(
        verify_node_info_claim(&claim, NOW - NODE_INFO_CLAIM_MAX_AGE_MS - 1),
        ClaimVerification::StaleClaim
    );
}

#[test]
fn verify_rejects_malformed() {
    let mut claim = sample_claim(true);
    claim.type_ = "other".to_string();
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::MalformedClaim
    );
    let mut claim = sample_claim(true);
    claim.version = 2;
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::MalformedClaim
    );
}

#[test]
fn verify_rejects_invalid_root_id() {
    let mut claim = sample_claim(true);
    claim.root_id = "zz".to_string();
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::InvalidRootId
    );
}

#[test]
fn verify_rejects_invalid_public_key_base64() {
    let mut claim = sample_claim(true);
    claim.public_key = "!!!not-base64!!!".to_string();
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::InvalidPublicKey
    );
}

#[test]
fn verify_rejects_public_key_root_mismatch() {
    // 换一个身份的公钥：base64 合法但 sha256 不匹配 rootId
    let other = parse_mnemonic("legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will").unwrap();
    let other_identity = derive_root_identity(&other.seed);
    let mut claim = sample_claim(true);
    claim.public_key = B64.encode(other_identity.public_key());
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::PublicKeyRootMismatch
    );
}

#[test]
fn verify_rejects_tampered_signature() {
    // 篡改签名内容
    let mut claim = sample_claim(true);
    claim.timestamp = NOW + 1;
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::InvalidSignature
    );
    // 签名长度非法
    let mut claim = sample_claim(true);
    claim.signature = B64.encode([0u8; 32]);
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::InvalidSignature
    );
    // 签名 base64 非法
    let mut claim = sample_claim(true);
    claim.signature = "###".to_string();
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::InvalidSignature
    );
    // 公钥长度非法（32 → 16 字节）：sha256 仍等于 rootId 不可能，
    // 所以落在 mismatch 分支——验证分支顺序
    let mut claim = sample_claim(true);
    claim.public_key = B64.encode([0u8; 16]);
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::PublicKeyRootMismatch
    );
}

#[test]
fn verify_checks_order_structure_before_stale() {
    // 结构错误优先于时间错误
    let mut claim = sample_claim(true);
    claim.version = 2;
    claim.timestamp = NOW - 60 * 60 * 1000;
    assert_eq!(
        verify_node_info_claim(&claim, NOW),
        ClaimVerification::MalformedClaim
    );
}

#[test]
fn custom_max_age() {
    let claim = sample_claim(true);
    assert!(verify_node_info_claim_with_max_age(&claim, NOW + 60_000, 120_000).is_ok());
    assert_eq!(
        verify_node_info_claim_with_max_age(&claim, NOW + 60_000, 30_000),
        ClaimVerification::StaleClaim
    );
}
