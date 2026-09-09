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

    let unlocked = unlock_inner(&mut kernel, PASSWORD, None, false).unwrap();
    assert_eq!(unlocked.root_id, init.root_id);
    assert_eq!(
        unlock_inner(&mut kernel, "wrong-password", None, false).unwrap_err(),
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
fn backup_payload_qr_roundtrip() {
    let (_dir1, mut kernel_a) = temp_kernel();
    let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();

    // 密码错误 → Invalid password（与 reveal_mnemonic 同口径）
    assert_eq!(
        backup_payload_qr_inner(&kernel_a, "wrong-password").unwrap_err(),
        "Invalid password"
    );
    // v2 紧凑载荷瘦身（无头像、无 publicKeyHex、hex→base64）：本测试
    // p2p=None，产出的是无 {v,i,p,a} 封装的裸紧凑 JSON，实测 ~554B。
    // 阈值对齐设计 §9 S6（<1000B，无 pk）——仍给更长昵称与 pwv 注入留余量。
    let qr = backup_payload_qr_inner(&kernel_a, PASSWORD).unwrap();
    assert!(
        qr.payload.len() < 1000,
        "v2 紧凑载荷应 <1000B（实测 {}B）",
        qr.payload.len()
    );

    let (_dir2, mut kernel_b) = temp_kernel();
    let recovered = recover_backup_inner(&mut kernel_b, &qr.payload, PASSWORD).unwrap();
    assert_eq!(recovered.root_id, init.root_id);
}

#[test]
fn v2_code_rejected_by_v1_only_reader_fail_closed() {
    // 模拟「只懂 v1 的恢复端」遇到 v2 码（设计 §6 兼容矩阵）：
    // v1 端把整个 payload 当磁盘 IdentityFile 解析，v2 紧凑码缺必填的
    // publicKeyHex → 反序列化失败 → 报「备份数据无效或已损坏」，
    // 绝不静默错恢复（fail-closed）。
    let (_dir1, mut kernel_a) = temp_kernel();
    init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();
    let v2_qr = backup_payload_qr_inner(&kernel_a, PASSWORD).unwrap();

    // v1 旧端解析路径 = IdentityFile::from_json(整个载荷)。v2 紧凑码必被拒。
    let parse_result =
        spark_core::identity::file::IdentityFile::from_json(&v2_qr.payload);
    assert!(
        parse_result.is_err(),
        "v1-only 恢复端不得解析 v2 紧凑码（fail-closed，不得静默错恢复）"
    );

    // 旧端把 from_json 失败映射为「备份数据无效或已损坏」（recover_backup
    // 的 parse_err），该文案已在 recover_backup_roundtrip 的损坏载荷断言里锁定。
}

#[test]
fn v1_code_read_by_new_restore_is_byte_identical() {
    // 旧（v1 码，hex + publicKeyHex）→ 新恢复端正常恢复，字节级一致（§6）。
    let (_dir1, mut kernel_a) = temp_kernel();
    let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();
    let src = current_identity_inner(&kernel_a).unwrap().unwrap();

    // v1 码两种形态：
    // 1) 裸磁盘 IdentityFile JSON（backup_payload，顶层无 v 的旧版遗留形态）；
    // 2) 顶层 {v:1, i: <IdentityFile JSON>} 封装。
    let bare_v1 = backup_payload_inner(&kernel_a).unwrap().payload;
    let wrapped_v1 = serde_json::json!({
        "v": 1,
        "i": serde_json::from_str::<serde_json::Value>(&bare_v1).unwrap(),
    })
    .to_string();

    for v1_code in [bare_v1, wrapped_v1] {
        let (_dir2, mut kernel_b) = temp_kernel();
        let recovered = recover_backup_inner(&mut kernel_b, &v1_code, PASSWORD).unwrap();
        assert_eq!(recovered.root_id, init.root_id, "v1 码恢复出同一 rootId");
        let cur = current_identity_inner(&kernel_b).unwrap().unwrap();
        assert_eq!(
            cur.public_key_hex, src.public_key_hex,
            "v1 码恢复的 publicKeyHex 与源字节级一致"
        );
    }
}

#[test]
fn v2_code_rebuilds_public_key_hex_on_disk_identity() {
    // v2 码不带 publicKeyHex；恢复端重建磁盘 IdentityFile 时以派生公钥补全
    // （§4.3）。命令层契约确认：恢复后当前身份的 publicKeyHex 与源字节级一致。
    let (_dir1, mut kernel_a) = temp_kernel();
    let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();
    let src = current_identity_inner(&kernel_a).unwrap().unwrap();
    let v2_qr = backup_payload_qr_inner(&kernel_a, PASSWORD).unwrap();

    let (_dir2, mut kernel_b) = temp_kernel();
    let recovered = recover_backup_inner(&mut kernel_b, &v2_qr.payload, PASSWORD).unwrap();
    assert_eq!(recovered.root_id, init.root_id);

    let cur = current_identity_inner(&kernel_b).unwrap().unwrap();
    assert_eq!(
        cur.public_key_hex, src.public_key_hex,
        "v2 码恢复后磁盘 IdentityFile 的 publicKeyHex 应为派生公钥（与源一致）"
    );
    assert_eq!(
        cur.public_key_hex.len(),
        src.public_key_hex.len(),
        "publicKeyHex 为 64 字符 hex"
    );
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

#[test]
fn change_password_updates_unlock_and_session() {
    let (_dir, mut kernel) = temp_kernel();
    let init = init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();
    const NEW_PW: &str = "new-password-321";

    // 未解锁态调用 → Root identity is locked
    lock_inner(&mut kernel);
    assert_eq!(
        change_password_inner(&mut kernel, PASSWORD, NEW_PW).unwrap_err(),
        "Root identity is locked"
    );

    // 解锁后旧口令错误 → Invalid password（与 reveal_mnemonic 同口径）
    unlock_inner(&mut kernel, PASSWORD, None, false).unwrap();
    assert_eq!(
        change_password_inner(&mut kernel, "wrong-password", NEW_PW).unwrap_err(),
        "Invalid password"
    );

    // 改密成功返回 { success: true }
    assert!(change_password_inner(&mut kernel, PASSWORD, NEW_PW).unwrap().success);

    // 旧口令 unlock 失败、新口令 unlock 成功
    lock_inner(&mut kernel);
    assert_eq!(
        unlock_inner(&mut kernel, PASSWORD, None, false).unwrap_err(),
        "Invalid password"
    );
    assert_eq!(unlock_inner(&mut kernel, NEW_PW, None, false).unwrap().root_id, init.root_id);

    // 改密后会话缓存口令已更新：免密码 update_profile_session 仍工作
    let profile =
        update_profile_inner(&mut kernel, Some("alice-renamed"), None, None, None, None).unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("alice-renamed"));
    // 资料更新后仍可用新口令解锁
    lock_inner(&mut kernel);
    assert_eq!(unlock_inner(&mut kernel, NEW_PW, None, false).unwrap().root_id, init.root_id);
}

#[test]
fn change_password_short_and_empty_new_password_rejected() {
    // 内核 change_password 走 check_password 强度校验（对齐 init/recover ≥8 位）：
    // 短/空新口令被拒绝（Error invoking 包装的错误文案为 Password must be at least 8 characters）。
    let (_dir, mut kernel) = temp_kernel();
    init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();

    assert_eq!(
        change_password_inner(&mut kernel, PASSWORD, "short").unwrap_err(),
        "Password must be at least 8 characters"
    );
    assert_eq!(
        change_password_inner(&mut kernel, PASSWORD, "").unwrap_err(),
        "Password must be at least 8 characters"
    );
    // 文件未被改密：原口令仍可解锁
    lock_inner(&mut kernel);
    assert_eq!(unlock_inner(&mut kernel, PASSWORD, None, false).unwrap().root_id.is_empty(), false);
}
