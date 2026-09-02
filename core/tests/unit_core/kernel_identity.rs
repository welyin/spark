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

const NEW_PASSWORD: &str = "new-password-321";

#[test]
fn change_password_updates_unlock_and_session_cache() {
    let (_dir, mut kernel) = temp_kernel();
    let init = kernel.init_identity(PASSWORD, "alice", None).unwrap();

    // 未解锁态调用 → Locked
    kernel.lock();
    assert_eq!(
        kernel
            .change_password(PASSWORD, NEW_PASSWORD)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    // 解锁后：旧口令错误 → Invalid password（与 reveal_mnemonic 同口径）
    kernel.unlock(PASSWORD, None).unwrap();
    assert_eq!(
        kernel
            .change_password("wrong-password", NEW_PASSWORD)
            .unwrap_err()
            .to_string(),
        "Invalid password"
    );

    // 改密成功
    kernel.change_password(PASSWORD, NEW_PASSWORD).unwrap();
    assert!(kernel.status().unwrap().unlocked);

    // 旧口令 unlock 失败、新口令 unlock 成功（文件确已用新口令封存）
    kernel.lock();
    assert_eq!(
        kernel.unlock(PASSWORD, None).unwrap_err().to_string(),
        "Invalid password"
    );
    let root_id = kernel.unlock(NEW_PASSWORD, None).unwrap();
    assert_eq!(root_id, init.root_id);

    // 改密后 unlock 会话缓存已更新：免密码 update_profile_session 仍工作
    // （依赖会话缓存口令/密钥重封，若未更新会以旧口令把文件封回去）
    let profile = kernel
        .update_profile_session(Some("alice-renamed"), None, None, None, None)
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("alice-renamed"));
    // 资料更新后仍可解锁、改密前数据未损
    kernel.lock();
    assert_eq!(kernel.unlock(NEW_PASSWORD, None).unwrap(), init.root_id);
}

#[test]
fn change_password_same_password_is_noop_roundtrip() {
    let (_dir, mut kernel) = temp_kernel();
    let init = kernel.init_identity(PASSWORD, "alice", None).unwrap();

    // 新旧口令相同：合法操作（无意义但不应报错/损坏文件），改后仍可用原口令解锁
    kernel.change_password(PASSWORD, PASSWORD).unwrap();
    assert!(kernel.status().unwrap().unlocked);
    kernel.lock();
    let root_id = kernel.unlock(PASSWORD, None).unwrap();
    assert_eq!(root_id, init.root_id);
}

#[test]
fn change_password_empty_new_password_rejected() {
    // change_password 走 check_password 强度校验（对齐 init/recover ≥8 位）：
    // 空新口令被内核拒绝，封存口令不变，原口令仍可解锁。
    let (_dir, mut kernel) = temp_kernel();
    let init = kernel.init_identity(PASSWORD, "alice", None).unwrap();

    assert_eq!(
        kernel
            .change_password(PASSWORD, "")
            .unwrap_err()
            .to_string(),
        "Password must be at least 8 characters"
    );
    // 文件未被改密：旧口令仍可解锁
    kernel.lock();
    assert_eq!(kernel.unlock(PASSWORD, None).unwrap(), init.root_id);
}

#[test]
fn change_password_short_new_password_rejected() {
    // <8 位新口令同被 check_password 拒绝（对齐 init/recover 强度口径），
    // 封存口令不变。
    let (_dir, mut kernel) = temp_kernel();
    let init = kernel.init_identity(PASSWORD, "alice", None).unwrap();

    assert_eq!(
        kernel
            .change_password(PASSWORD, "short")
            .unwrap_err()
            .to_string(),
        "Password must be at least 8 characters"
    );
    kernel.lock();
    assert_eq!(kernel.unlock(PASSWORD, None).unwrap(), init.root_id);
}
