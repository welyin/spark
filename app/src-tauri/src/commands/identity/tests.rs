//! 身份命令单测：直调 *_inner，不依赖 WebView。

use super::*;

const PASSWORD: &str = "correct-horse-battery";

fn temp_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::init(spark_core::kernel::KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    (dir, kernel)
}

#[test]
fn status_on_fresh_dir_is_uninitialized() {
    let (_dir, kernel) = temp_kernel();
    let status = status_inner(&kernel).unwrap();
    assert!(!status.initialized);
    assert!(!status.unlocked);
    assert_eq!(status.root_id, None);
    assert!(list_identities_inner(&kernel).unwrap().is_empty());
    // 未初始化：依赖当前身份的命令报 NotInitialized 文案
    assert_eq!(
        backup_payload_inner(&kernel).unwrap_err(),
        "Root identity is not initialized"
    );
    assert!(current_identity_inner(&kernel).unwrap().is_none());
}

#[test]
fn full_identity_lifecycle() {
    let (_dir, mut kernel) = temp_kernel();

    // init：返回 rootId + 24 词助记词，身份随即解锁
    let init = init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();
    assert!(!init.root_id.is_empty());
    assert_eq!(init.mnemonic.split_whitespace().count(), 24);

    let status = status_inner(&kernel).unwrap();
    assert!(status.initialized && status.unlocked);
    assert_eq!(status.root_id.as_deref(), Some(init.root_id.as_str()));
    assert_eq!(status.nickname.as_deref(), Some("alice"));

    let list = list_identities_inner(&kernel).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].active);
    assert_eq!(list[0].root_id, init.root_id);

    let current = current_identity_inner(&kernel).unwrap().unwrap();
    assert_eq!(current.root_id, init.root_id);
    assert!(!current.public_key_hex.is_empty());

    // reveal_mnemonic：密码门控，错误密码报 Invalid password
    assert_eq!(
        reveal_mnemonic_inner(&kernel, "wrong-password").unwrap_err(),
        "Invalid password"
    );
    let revealed = reveal_mnemonic_inner(&kernel, PASSWORD).unwrap();
    assert_eq!(revealed.mnemonic, init.mnemonic);

    // update_profile（免密码会话版）：改昵称、清头像（B1：清除以空串表达，
    // present-but-null 在 IPC 边界会坍塌为 None 永远到不了内核）
    let profile = update_profile_inner(&mut kernel, Some("alice-2"), Some(""), None, None, None).unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("alice-2"));
    assert_eq!(profile.avatar, None);

    // backup_payload：返回当前身份密文 JSON
    let backup = backup_payload_inner(&kernel).unwrap();
    assert!(backup.payload.contains(&init.root_id));

    // lock → status 反映锁定；解锁后恢复
    lock_inner(&mut kernel);
    let status = status_inner(&kernel).unwrap();
    assert!(status.initialized && !status.unlocked);
    assert!(current_identity_inner(&kernel).unwrap().is_none());

    let unlocked = unlock_inner(&mut kernel, PASSWORD, None).unwrap();
    assert_eq!(unlocked.root_id, init.root_id);
    assert_eq!(
        unlock_inner(&mut kernel, "wrong-password", None).unwrap_err(),
        "Invalid password"
    );
}

#[test]
fn recover_mnemonic_on_second_device_and_set_active() {
    let (_dir1, mut kernel_a) = temp_kernel();
    let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();

    // 另一"设备"（独立数据目录）：助记词恢复出同一 rootId
    let (_dir2, mut kernel_b) = temp_kernel();
    let recovered =
        recover_mnemonic_inner(&mut kernel_b, &init.mnemonic, "new-password-1", "alice-b", None)
            .unwrap();
    assert_eq!(recovered.root_id, init.root_id);

    // 重复恢复同一身份报"已在本设备上"
    assert!(recover_mnemonic_inner(
        &mut kernel_b,
        &init.mnemonic,
        "new-password-1",
        "x",
        None
    )
    .unwrap_err()
    .contains("已在本设备上"));

    // 坏助记词报校验失败文案
    assert!(recover_mnemonic_inner(&mut kernel_b, "一二三四", "new-password-1", "x", None)
        .unwrap_err()
        .contains("助记词校验失败"));

    // 同目录第二个身份 + set_active 切换指针
    let second = init_inner(&mut kernel_a, PASSWORD, "bob", None).unwrap();
    assert_ne!(second.root_id, init.root_id);
    lock_inner(&mut kernel_a);
    set_active_inner(&kernel_a, &init.root_id).unwrap();
    let list = list_identities_inner(&kernel_a).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|i| i.root_id == init.root_id && i.active));
    set_active_inner(&kernel_a, "no-such-root-id").unwrap_err();
}

#[test]
fn recover_backup_roundtrip() {
    let (_dir1, mut kernel_a) = temp_kernel();
    let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();
    let backup = backup_payload_inner(&kernel_a).unwrap();

    let (_dir2, mut kernel_b) = temp_kernel();
    // 密码错误 → 专用文案
    assert_eq!(
        recover_backup_inner(&mut kernel_b, &backup.payload, "wrong-password").unwrap_err(),
        "密码不正确"
    );
    // 载荷损坏 → 专用文案
    assert_eq!(
        recover_backup_inner(&mut kernel_b, "{not-json", PASSWORD).unwrap_err(),
        "备份数据无效或已损坏"
    );
    let recovered = recover_backup_inner(&mut kernel_b, &backup.payload, PASSWORD).unwrap();
    assert_eq!(recovered.root_id, init.root_id);
    assert!(status_inner(&kernel_b).unwrap().unlocked);
}

#[test]
fn password_policy_enforced() {
    let (_dir, mut kernel) = temp_kernel();
    assert_eq!(
        init_inner(&mut kernel, "short", "alice", None).unwrap_err(),
        "Password must be at least 8 characters"
    );
}

#[test]
fn sign_derive_domain_and_mnemonic_check() {
    let (_dir, mut kernel) = temp_kernel();

    // 锁定状态
    assert_eq!(sign_inner(&kernel, "p").unwrap_err(), "Root identity is locked");
    assert_eq!(
        derive_domain_inner(&kernel, "plugin:chat").unwrap_err(),
        "Root identity is locked"
    );

    let init = init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();

    // sign：rootId/payloadHash 形状
    let sig = sign_inner(&kernel, "hello").unwrap();
    assert_eq!(sig.root_id, init.root_id);
    assert_eq!(sig.payload_hash.len(), 64);
    assert!(!sig.signature.is_empty());

    // derive：确定性 + 域回显 + 空域报错
    let d1 = derive_domain_inner(&kernel, "plugin:chat").unwrap();
    let d2 = derive_domain_inner(&kernel, "plugin:chat").unwrap();
    assert_eq!(d1, d2);
    assert_eq!(d1.domain, "plugin:chat");
    assert!(d1.derivation_path.starts_with("m/44'/607'/0'/0'/0'/"));
    assert_eq!(
        derive_domain_inner(&kernel, "  ").unwrap_err(),
        "Domain is required"
    );

    // mnemonic-check：词数组 + 词表外词下标
    let check = mnemonic_check_inner("legal winner notaword");
    assert_eq!(check.words.len(), 3);
    assert_eq!(check.invalid_indexes, vec![2]);
    let continuous = mnemonic_check_inner("与祝产");
    assert_eq!(continuous.words, vec!["与", "祝", "产"]);
    assert!(continuous.invalid_indexes.is_empty());
}

#[test]
fn update_profile_extra_fields_patch_semantics() {
    let (_dir, mut kernel) = temp_kernel();
    init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();

    // 设置扩展字段；昵称/头像不变
    let profile =
        update_profile_inner(&mut kernel, None, None, Some("女"), Some("杭州"), Some("保持热爱"))
            .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("alice"));
    assert_eq!(profile.gender.as_deref(), Some("女"));
    assert_eq!(profile.region.as_deref(), Some("杭州"));
    assert_eq!(profile.signature.as_deref(), Some("保持热爱"));

    // 缺省（None）= 不变；空串 = 清除
    let profile = update_profile_inner(&mut kernel, None, None, Some(""), None, None).unwrap();
    assert_eq!(profile.gender, None);
    assert_eq!(profile.region.as_deref(), Some("杭州"));
    assert_eq!(profile.signature.as_deref(), Some("保持热爱"));

    // avatar：设置后空串清除（B1：锁死 IPC 边界 Some("") = 清除）
    let profile = update_profile_inner(
        &mut kernel,
        None,
        Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg=="),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        profile.avatar.as_deref(),
        Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==")
    );
    let profile = update_profile_inner(&mut kernel, None, Some(""), None, None, None).unwrap();
    assert_eq!(profile.avatar, None);

    // root_status 视图回读扩展字段
    let status = status_inner(&kernel).unwrap();
    assert_eq!(status.gender, None);
    assert_eq!(status.region.as_deref(), Some("杭州"));
    assert_eq!(status.signature.as_deref(), Some("保持热爱"));
}
