//! kernel 身份门面集成测试：注册/解锁/资料更新、备份码与助记词恢复、
//! 签名/域派生/助记词校验、会话版资料更新。

mod common;

use serde_json::Value;
use spark_core::kernel::{Kernel, KernelError};

use common::*;

// ---------------------------------------------------------------------------
// 身份全流程：init → 重启 → unlock → update_profile → list → 备份/助记词恢复
// ---------------------------------------------------------------------------

#[test]
fn identity_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 初始状态：无身份
    let status = kernel.status().unwrap();
    assert!(!status.initialized && !status.unlocked && status.root_id.is_none());
    assert!(kernel.list_identities().unwrap().is_empty());

    // 注册
    let (root_id, mnemonic) = init_identity(&mut kernel);
    assert_eq!(mnemonic.split_whitespace().count(), 24, "24 词助记词");

    // 目录结构与 TS 对齐：identities/{rootId}.json + active-identity.json
    let identity_path = dir
        .path()
        .join("identities")
        .join(format!("{root_id}.json"));
    assert!(identity_path.exists());
    let active_raw = std::fs::read_to_string(dir.path().join("active-identity.json")).unwrap();
    assert_eq!(
        active_raw,
        format!(r#"{{"activeRootId":"{root_id}"}}"#),
        "活动指针逐字节对齐 TS"
    );

    // 身份文件：两空格缩进（JSON.stringify(payload, null, 2) 风格）、v2 字段齐备
    let file_raw = std::fs::read_to_string(&identity_path).unwrap();
    assert!(
        file_raw.starts_with("{\n  \"version\": 2,"),
        "两空格缩进、version 为首字段"
    );
    let file_json: Value = serde_json::from_str(&file_raw).unwrap();
    assert_eq!(file_json["kdf"], "scrypt");
    assert_eq!(file_json["rootId"], root_id);
    assert_eq!(file_json["nickname"], "小明", "昵称已 trim");
    assert!(file_json["authTag"].is_string() && file_json["publicKeyHex"].is_string());

    // 存储目录按身份对齐
    let storage_dir = kernel.storage_dir().expect("storage open");
    assert_eq!(
        storage_dir.file_name().unwrap().to_string_lossy(),
        format!("spark-sled-{}", &root_id[..16])
    );
    assert!(storage_dir.exists());

    // status / list
    let status = kernel.status().unwrap();
    assert!(status.initialized && status.unlocked);
    assert_eq!(status.root_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(status.nickname.as_deref(), Some("小明"));
    let list = kernel.list_identities().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].active && list[0].nickname.as_deref() == Some("小明"));

    // 助记词获取（密码门控）
    assert_eq!(kernel.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);
    let err = kernel.reveal_mnemonic("wrong-password").unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));
    assert_eq!(err.to_string(), "Invalid password");

    // 更新资料
    let profile = kernel.update_profile(PASSWORD, Some("小红"), None).unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小红"));
    assert_eq!(kernel.status().unwrap().nickname.as_deref(), Some("小红"));
    let err = kernel
        .update_profile("wrong-password", Some("x"), None)
        .unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));

    // 当前身份公开信息
    let public = kernel.current_identity().unwrap().expect("unlocked");
    assert_eq!(public.root_id, root_id);
    assert_eq!(public.nickname.as_deref(), Some("小红"));
    assert_eq!(public.public_key_hex.len(), 64);

    // 锁定后再解锁
    kernel.lock();
    assert!(kernel.current_identity().unwrap().is_none());
    assert!(!kernel.status().unwrap().unlocked);
    let err = kernel.unlock("wrong-password", None).unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));
    assert_eq!(kernel.unlock(PASSWORD, None).unwrap(), root_id);
    assert!(kernel.status().unwrap().unlocked);

    kernel.shutdown().unwrap();

    // 重启：活动身份恢复，存储重开，未解锁
    let mut kernel = fresh_kernel(dir.path());
    let status = kernel.status().unwrap();
    assert!(status.initialized && !status.unlocked);
    assert_eq!(status.root_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(status.nickname.as_deref(), Some("小红"));
    // 解锁指定 rootId
    assert_eq!(kernel.unlock(PASSWORD, Some(&root_id)).unwrap(), root_id);
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 备份码与助记词恢复（跨目录 = 跨设备语义）
// ---------------------------------------------------------------------------

#[test]
fn identity_backup_and_mnemonic_recovery() {
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let (root_id, mnemonic) = init_identity(&mut kernel_a);

    // 备份码 = 当前身份密文记录的紧凑 JSON
    let backup = kernel_a.backup_payload().unwrap();
    let backup_json: Value = serde_json::from_str(&backup).unwrap();
    assert_eq!(backup_json["rootId"], root_id);
    assert!(!backup.contains("\n"), "备份载荷为紧凑 JSON");

    // 设备 B：备份码恢复
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    let err = kernel_b
        .recover_backup(&backup, "wrong-password")
        .unwrap_err();
    assert_eq!(err.to_string(), "密码不正确");
    let err = kernel_b.recover_backup("not-json", PASSWORD).unwrap_err();
    assert_eq!(err.to_string(), "备份数据无效或已损坏");
    assert_eq!(kernel_b.recover_backup(&backup, PASSWORD).unwrap(), root_id);
    assert!(kernel_b.status().unwrap().unlocked);
    // 同一设备重复恢复 → 拒绝
    let err = kernel_b.recover_backup(&backup, PASSWORD).unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    kernel_b.shutdown().unwrap();

    // 设备 C：助记词恢复（连续书写无空格，中文词表）
    let dir_c = tempfile::tempdir().unwrap();
    let mut kernel_c = fresh_kernel(dir_c.path());
    let continuous: String = mnemonic.chars().filter(|c| !c.is_whitespace()).collect();
    let err = kernel_c
        .recover_mnemonic(&continuous, PASSWORD, "恢复用户", None)
        .unwrap();
    assert_eq!(err, root_id, "连续书写的中文助记词可恢复同一身份");
    // 空格分隔形式 + 错误助记词
    let err = kernel_c
        .recover_mnemonic(&mnemonic, PASSWORD, "恢复用户", None)
        .unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    let err = kernel_c
        .recover_mnemonic("abandon abandon abandon", PASSWORD, "x", None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "助记词校验失败：请检查是否有错别字、漏字或顺序错误"
    );
    kernel_c.shutdown().unwrap();

    // 设备 A：同一助记词恢复 → 拒绝（已在本设备）
    let err = kernel_a
        .recover_mnemonic(&mnemonic, PASSWORD, "x", None)
        .unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    kernel_a.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 阶段③c 新增门面：签名/域派生/助记词校验、会话版资料更新
// ---------------------------------------------------------------------------

#[test]
fn sign_and_derive_domain_identity() {
    use base64::Engine as _;
    use ed25519_dalek::Verifier as _;

    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 锁定状态：sign/derive 报 Locked
    assert_eq!(
        kernel.sign("payload").unwrap_err().to_string(),
        "Root identity is locked"
    );
    assert_eq!(
        kernel
            .derive_domain_identity("plugin:chat")
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    let (root_id, mnemonic) = init_identity(&mut kernel);

    // sign：rootId 一致、签名可用根公钥验过、payloadHash = sha256hex(utf8 字节)
    let sig = kernel.sign("hello spark").unwrap();
    assert_eq!(sig.root_id, root_id);
    assert_eq!(
        sig.payload_hash,
        spark_core::evidence::sha256_hex("hello spark")
    );
    let public = kernel.current_identity().unwrap().unwrap();
    let pub_bytes = hex::decode(public.public_key_hex).unwrap();
    let verifying =
        ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes.try_into().unwrap()).unwrap();
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sig.signature)
        .unwrap();
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes.try_into().unwrap());
    verifying.verify(b"hello spark", &signature).unwrap();

    // derive：与 identity 模块由助记词种子派生的结果一致；空域报 TS 文案
    let derived = kernel.derive_domain_identity("plugin:chat").unwrap();
    assert_eq!(derived.domain, "plugin:chat");
    let seed = spark_core::identity::parse_mnemonic(&mnemonic)
        .unwrap()
        .seed;
    let expected = spark_core::identity::derive_domain_identity(&seed, "plugin:chat");
    assert_eq!(derived.domain_id, expected.id());
    assert_eq!(
        derived.public_key,
        base64::engine::general_purpose::STANDARD.encode(expected.public_key())
    );
    assert_eq!(derived.derivation_path, expected.path);
    assert_eq!(
        kernel
            .derive_domain_identity("   ")
            .unwrap_err()
            .to_string(),
        "Domain is required"
    );

    kernel.shutdown().unwrap();
}

#[test]
fn check_mnemonic_word_validation() {
    // 纯函数：空格分隔中文词全在词表 → 无非法下标
    let ok = Kernel::check_mnemonic("与 祝 产 鸡 永 烂");
    assert_eq!(ok.words, vec!["与", "祝", "产", "鸡", "永", "烂"]);
    assert!(ok.invalid_indexes.is_empty());

    // 连续书写（无空白）按单字拆分
    let continuous = Kernel::check_mnemonic("与祝产");
    assert_eq!(continuous.words, vec!["与", "祝", "产"]);

    // 英文词表词同样合法；混合非法词给出下标
    let mixed = Kernel::check_mnemonic("legal winner notaword 与");
    assert_eq!(mixed.invalid_indexes, vec![2]);

    // 无空白拉丁串按单字拆，单字不在任何词表 → 全部非法
    let latin = Kernel::check_mnemonic("abc");
    assert_eq!(latin.words, vec!["a", "b", "c"]);
    assert_eq!(latin.invalid_indexes, vec![0, 1, 2]);

    // 空输入
    let empty = Kernel::check_mnemonic("   ");
    assert!(empty.words.is_empty() && empty.invalid_indexes.is_empty());
}

#[test]
fn update_profile_session_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 无会话（未解锁）→ Locked
    assert_eq!(
        kernel
            .update_profile_session(Some("x"), None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    let (root_id, mnemonic) = init_identity(&mut kernel);
    let avatar = "data:image/png;base64,iVBORw0KGgo=";

    // 会话版：免密码改昵称 + 设头像
    let profile = kernel
        .update_profile_session(Some("  小明二号  "), Some(Some(avatar)))
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小明二号"));
    assert_eq!(profile.avatar.as_deref(), Some(avatar));
    let status = kernel.status().unwrap();
    assert_eq!(status.nickname.as_deref(), Some("小明二号"));
    assert_eq!(status.avatar.as_deref(), Some(avatar));

    // 清头像（恢复自动头像）；昵称不变
    let profile = kernel.update_profile_session(None, Some(None)).unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小明二号"));
    assert_eq!(profile.avatar, None);

    // 非法昵称报错
    assert!(
        kernel
            .update_profile_session(Some(&"长".repeat(25)), None)
            .is_err()
    );

    // lock 清除会话 → 再调报 Locked
    kernel.lock();
    assert_eq!(
        kernel
            .update_profile_session(Some("x"), None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    // 重新解锁：资料保持（重封未破坏文件），助记词仍可用原密码查看
    kernel.unlock(PASSWORD, None).unwrap();
    let status = kernel.status().unwrap();
    assert_eq!(status.nickname.as_deref(), Some("小明二号"));
    assert_eq!(kernel.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);
    let _ = root_id;
    kernel.shutdown().unwrap();
}
