//! 单元测试：域身份签名（`plugin-identity-sign` 的内核侧）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use spark_core::identity;
use spark_core::kernel::{Kernel, KernelConfig};

const PASSWORD: &str = "correct-horse-battery";
const DOMAIN: &str = "plugin:spark-example";
const PAYLOAD: &str = "org_123:post_456:hello spark";

fn temp_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    (dir, kernel)
}

#[test]
fn sign_with_domain_identity_roundtrip() {
    let (_dir, mut kernel) = temp_kernel();

    // 锁定 → TS `Root identity is locked`
    assert_eq!(
        kernel
            .sign_with_domain_identity(DOMAIN, PAYLOAD)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    kernel.init_identity(PASSWORD, "alice", None).unwrap();

    // 固定载荷签名：形状齐全，验签通过
    let sig = kernel.sign_with_domain_identity(DOMAIN, PAYLOAD).unwrap();
    assert_eq!(sig.domain, DOMAIN);
    assert_eq!(sig.payload_hash, spark_core::evidence::sha256_hex(PAYLOAD));
    assert!(identity::verify_ed25519_signature(
        PAYLOAD,
        &sig.signature,
        &sig.public_key
    ));

    // domainId 与 derive_domain_identity 一致（= sha256hex(域公钥)）
    let derived = kernel.derive_domain_identity(DOMAIN).unwrap();
    assert_eq!(sig.domain_id, derived.domain_id);
    assert_eq!(sig.public_key, derived.public_key);

    // 确定性：同域同载荷再签结果一致
    let sig2 = kernel.sign_with_domain_identity(DOMAIN, PAYLOAD).unwrap();
    assert_eq!(sig, sig2);

    // 与 root sign 区分：签名者公钥/签名均不同
    let root_sig = kernel.sign(PAYLOAD).unwrap();
    let root_public_key_hex = kernel.current_identity().unwrap().unwrap().public_key_hex;
    assert_ne!(
        B64.decode(&sig.public_key).unwrap(),
        hex::decode(root_public_key_hex).unwrap()
    );
    assert_ne!(sig.signature, root_sig.signature);
    // root 签名用域公钥验不过，域签名亦然（payloadHash 口径一致，仅签名者不同）
    assert!(!identity::verify_ed25519_signature(
        PAYLOAD,
        &root_sig.signature,
        &sig.public_key
    ));

    // 不同域 → 不同域公钥；篡改载荷验签失败
    let other = kernel
        .sign_with_domain_identity("plugin:chat", PAYLOAD)
        .unwrap();
    assert_ne!(other.public_key, sig.public_key);
    assert!(!identity::verify_ed25519_signature(
        "tampered",
        &sig.signature,
        &sig.public_key
    ));
    // 坏 base64 / 长度不符 → false（不 panic）
    assert!(!identity::verify_ed25519_signature(
        PAYLOAD,
        "!!!",
        &sig.public_key
    ));
    assert!(!identity::verify_ed25519_signature(
        PAYLOAD,
        &sig.signature,
        "aGk="
    ));

    // 空域 → TS `Domain is required`
    assert_eq!(
        kernel
            .sign_with_domain_identity("  ", PAYLOAD)
            .unwrap_err()
            .to_string(),
        "Domain is required"
    );
}
