//! golden vectors 验收测试：加载 `../spec/vectors/identity.json` 逐条断言。
//!
//! 覆盖 identity.md §7：
//! 1. 中文 mnemonic → seedHex / publicKeyHex / rootId
//! 2. 两个 domain → idxA / idxB / 完整路径 / 域公钥 / domainId
//! 3. 英文 mnemonic → rootId（恢复兼容路径，词表探测）
//! 4. scrypt v2 固定 password+salt+iv → ciphertext/authTag 精确匹配 + 解密往返
//! 5. pbkdf2 v1 固定值 → ciphertext 精确匹配 + 解密往返

use spark_core::identity::crypto::{
    decrypt_v1, decrypt_v2, encrypt_v1, encrypt_v2, pbkdf2_v1_key, scrypt_v2_key,
};
use spark_core::identity::derive::{derive_domain_identity, derive_root_identity, domain_indices};
use spark_core::identity::file::{
    CompactBackupFile, IdentityFile, decode_compact_backup, migrate_v1_to_v2, unlock_identity,
    validate_nickname,
};
use spark_core::identity::mnemonic::{Wordlist, parse_mnemonic};
use spark_core::identity::slip10::format_derivation_path;

fn vectors() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/identity.json");
    let raw = std::fs::read_to_string(path).expect("read identity vectors");
    serde_json::from_str(&raw).expect("parse identity vectors")
}

#[test]
fn root_identity_chinese() {
    let v = vectors();
    let rv = &v["rootIdentityChinese"];

    let parsed = parse_mnemonic(rv["mnemonic"].as_str().unwrap()).expect("parse chinese mnemonic");
    assert_eq!(parsed.wordlist, Wordlist::ChineseSimplified);
    assert_eq!(
        parsed.mnemonic,
        rv["mnemonic"].as_str().unwrap(),
        "normalized mnemonic must round-trip"
    );
    assert_eq!(hex::encode(parsed.seed), rv["seedHex"].as_str().unwrap());

    let root = derive_root_identity(&parsed.seed);
    assert_eq!(root.path, rv["derivationPath"].as_str().unwrap());
    assert_eq!(root.public_key_hex(), rv["publicKeyHex"].as_str().unwrap());
    assert_eq!(root.id(), rv["rootId"].as_str().unwrap());
}

#[test]
fn domain_identities() {
    let v = vectors();
    let rv = &v["rootIdentityChinese"];
    let parsed = parse_mnemonic(rv["mnemonic"].as_str().unwrap()).unwrap();

    for dv in v["domainIdentities"].as_array().unwrap() {
        let domain = dv["domain"].as_str().unwrap();

        // 域哈希与索引
        let h = hex::encode(sha2_sm(domain));
        assert_eq!(
            h,
            dv["domainSha256Hex"].as_str().unwrap(),
            "sha256({domain})"
        );
        let (idx_a, idx_b) = domain_indices(domain);
        assert_eq!(idx_a, dv["idxA"].as_u64().unwrap() as u32, "idxA({domain})");
        assert_eq!(idx_b, dv["idxB"].as_u64().unwrap() as u32, "idxB({domain})");

        // 完整路径
        let identity = derive_domain_identity(&parsed.seed, domain);
        assert_eq!(identity.path, dv["derivationPath"].as_str().unwrap());
        assert_eq!(
            identity.public_key_hex(),
            dv["publicKeyHex"].as_str().unwrap()
        );
        assert_eq!(identity.id(), dv["domainId"].as_str().unwrap());
    }
}

#[test]
fn root_identity_english_v1_recovery() {
    let v = vectors();
    let rv = &v["rootIdentityEnglishV1"];

    // 恢复兼容路径：输入英文助记词，探测词表必须落到 english
    let parsed = parse_mnemonic(rv["mnemonic"].as_str().unwrap()).expect("parse english mnemonic");
    assert_eq!(parsed.wordlist, Wordlist::English);
    assert_eq!(hex::encode(parsed.seed), rv["seedHex"].as_str().unwrap());

    let root = derive_root_identity(&parsed.seed);
    assert_eq!(root.path, rv["derivationPath"].as_str().unwrap());
    assert_eq!(root.public_key_hex(), rv["publicKeyHex"].as_str().unwrap());
    assert_eq!(root.id(), rv["rootId"].as_str().unwrap());
}

#[test]
fn scrypt_v2_exact_and_roundtrip() {
    let v = vectors();
    let sv = &v["scryptV2"];
    let password = sv["password"].as_str().unwrap();
    let salt = hex::decode(sv["saltHex"].as_str().unwrap()).unwrap();
    let iv = hex::decode(sv["ivHex"].as_str().unwrap()).unwrap();
    let plaintext = sv["plaintextJson"].as_str().unwrap().as_bytes();

    // KDF 参数断言
    assert_eq!(sv["kdf"]["N"].as_u64().unwrap(), 32768);
    assert_eq!(sv["kdf"]["r"].as_u64().unwrap(), 8);
    assert_eq!(sv["kdf"]["p"].as_u64().unwrap(), 1);
    assert_eq!(sv["kdf"]["keyLen"].as_u64().unwrap(), 32);

    // 固定 password+salt+iv → 密文/authTag 精确匹配
    let (data, tag) = encrypt_v2(plaintext, password, &salt, &iv).unwrap();
    assert_eq!(hex::encode(&data), sv["ciphertextHex"].as_str().unwrap());
    assert_eq!(hex::encode(&tag), sv["authTagHex"].as_str().unwrap());

    // 解密往返
    let back = decrypt_v2(&data, &tag, password, &salt, &iv).unwrap();
    assert_eq!(back, plaintext);

    // 错误密码必须失败
    assert!(decrypt_v2(&data, &tag, "wrong-password", &salt, &iv).is_err());

    // KDF 确定性
    let k1 = scrypt_v2_key(password, &salt).unwrap();
    let k2 = scrypt_v2_key(password, &salt).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn pbkdf2_v1_exact_and_roundtrip() {
    let v = vectors();
    let pv = &v["pbkdf2V1"];
    let password = pv["password"].as_str().unwrap();
    let salt = hex::decode(pv["saltHex"].as_str().unwrap()).unwrap();
    let iv = hex::decode(pv["ivHex"].as_str().unwrap()).unwrap();
    let plaintext = pv["plaintextJson"].as_str().unwrap().as_bytes();

    assert_eq!(pv["kdf"]["iterations"].as_u64().unwrap(), 210000);
    assert_eq!(pv["kdf"]["digest"].as_str().unwrap(), "sha512");
    assert_eq!(pv["kdf"]["keyLen"].as_u64().unwrap(), 32);

    let data = encrypt_v1(plaintext, password, &salt, &iv).unwrap();
    assert_eq!(hex::encode(&data), pv["ciphertextHex"].as_str().unwrap());

    let back = decrypt_v1(&data, password, &salt, &iv).unwrap();
    assert_eq!(back, plaintext);

    assert!(decrypt_v1(&data, "wrong-password", &salt, &iv).is_err());

    let k1 = pbkdf2_v1_key(password, &salt);
    let k2 = pbkdf2_v1_key(password, &salt);
    assert_eq!(k1, k2);
}

// =============================================================================
// 备份码 v2 紧凑格式向量（identity.md §7 条目 6–8；qr-backup-payload-compression §8.1）
//
// CompactBackupFile / build_compact_backup / decode_compact_backup 已落地；以下
// backup_code_v2_* 与 compact_backup_file_serialize_and_decode_framework 断言字节级
// 关系，其中框架测试核验 serde 实际输出字段序 + 重建磁盘文件 + 派生公钥补全。
// =============================================================================

/// 备份码 v2 载荷：base64 字段与 scryptV2 基准线形字节级一致（确定性）。
/// salt/iv/data/authTag/publicKey 的 base64 是同一份字节的不同编码，
/// 必须与 identity.json 的 `scryptV2`（hex）及 `backupCodeV2`（base64）逐字节对应。
#[test]
fn backup_code_v2_base64_matches_scrypt_benchmark() {
    let v = vectors();
    let bc = &v["backupCodeV2"];
    let sv = &v["scryptV2"];
    let rv = &v["rootIdentityChinese"];

    // base64 ↔ hex 同源往返（CompactBackupFile base64 ↔ IdentityFile hex）
    let b64 = |b: &[u8]| base64_encode(b);
    assert_eq!(
        b64(&hex::decode(sv["saltHex"].as_str().unwrap()).unwrap()),
        bc["saltBase64"]
    );
    assert_eq!(
        b64(&hex::decode(sv["ivHex"].as_str().unwrap()).unwrap()),
        bc["ivBase64"]
    );
    assert_eq!(
        b64(&hex::decode(sv["ciphertextHex"].as_str().unwrap()).unwrap()),
        bc["dataBase64"]
    );
    assert_eq!(
        b64(&hex::decode(sv["authTagHex"].as_str().unwrap()).unwrap()),
        bc["authTagBase64"]
    );
    // v2 码重建磁盘 IdentityFile 时补全的 publicKeyHex = 从助记词派生值（期望断言）
    assert_eq!(
        b64(&hex::decode(rv["publicKeyHex"].as_str().unwrap()).unwrap()),
        bc["expectedDerivedPublicKeyBase64"]
    );
    assert_eq!(
        rv["publicKeyHex"].as_str().unwrap(),
        bc["expectedDerivedPublicKeyHex"].as_str().unwrap()
    );

    // 反向：base64 → hex 回到磁盘 IdentityFile 的 hex 字段
    let hx = |s: &str| base64_decode(s);
    assert_eq!(
        hex::encode(hx(bc["saltBase64"].as_str().unwrap())),
        sv["saltHex"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(hx(bc["ivBase64"].as_str().unwrap())),
        sv["ivHex"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(hx(bc["dataBase64"].as_str().unwrap())),
        sv["ciphertextHex"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(hx(bc["authTagBase64"].as_str().unwrap())),
        sv["authTagHex"].as_str().unwrap()
    );
}

/// 备份码 v2 载荷可解密出与 scryptV2 相同的明文 payload（字节级）。
#[test]
fn backup_code_v2_decrypts_to_same_payload() {
    let v = vectors();
    let bc = &v["backupCodeV2"];
    let sv = &v["scryptV2"];

    let salt = base64_decode(bc["saltBase64"].as_str().unwrap());
    let iv = base64_decode(bc["ivBase64"].as_str().unwrap());
    let data = base64_decode(bc["dataBase64"].as_str().unwrap());
    let tag = base64_decode(bc["authTagBase64"].as_str().unwrap());
    let password = bc["password"].as_str().unwrap();

    let back = decrypt_v2(&data, &tag, password, &salt, &iv).unwrap();
    // 与磁盘 v2 向量同源明文（备份码重建磁盘文件后解锁结果一致）
    assert_eq!(back, sv["plaintextJson"].as_str().unwrap().as_bytes());
    // 错误密码必须失败（fail-closed）
    assert!(decrypt_v2(&data, &tag, "wrong", &salt, &iv).is_err());
}

/// 编码往返：`CompactBackupFile` base64 字段与 `IdentityFile` hex 字段同源字节往返一致。
/// 即：同一份字节先 hex 落磁盘、后 base64 进备份码，二者解码后字节完全一致。
#[test]
fn backup_code_v2_hex_base64_same_source_roundtrip() {
    let v = vectors();
    let bc = &v["backupCodeV2"];

    // 对 4 个加密封装字段逐一验证：hex→bytes→base64 与向量 base64 一致
    for (hex_f, b64_f) in [
        ("saltHex", "saltBase64"),
        ("ivHex", "ivBase64"),
        ("ciphertextHex", "dataBase64"),
        ("authTagHex", "authTagBase64"),
    ] {
        let hex_s = bc[hex_f].as_str().unwrap();
        let b64_s = bc[b64_f].as_str().unwrap();
        let bytes = hex::decode(hex_s).unwrap();
        assert_eq!(base64_encode(&bytes), b64_s, "{hex_f} → {b64_f}");
        assert_eq!(
            hex::encode(base64_decode(b64_s)),
            hex_s,
            "{b64_f} → {hex_f}"
        );
    }
}

/// CompactBackupFile 序列化线形核验（已启用）。
///
/// 用 backupCodeV2 固定值组装 `CompactBackupFile`，断言：
/// 1. `serde_json::to_value` 的字段序/字段集与设计 §4.2 逐字节一致；
/// 2. `decode_compact_backup` 把 v2 码重建磁盘 `IdentityFile`，base64→hex 字段回填
///    正确、补 version:2；
/// 3. 解锁后派生公钥补全 `publicKeyHex` = `rootIdentityChinese.publicKeyHex`（权威来源）。
#[test]
fn compact_backup_file_serialize_and_decode_framework() {
    let v = vectors();
    let bc = &v["backupCodeV2"];

    // 用 backupCodeV2 固定值组装 CompactBackupFile（与实现一致地 base64 化）。
    let compact = CompactBackupFile {
        v: bc["innerVersion"].as_u64().unwrap() as u32,
        kdf: bc["kdf"].as_str().unwrap().to_string(),
        salt: bc["saltBase64"].as_str().unwrap().to_string(),
        iv: bc["ivBase64"].as_str().unwrap().to_string(),
        data: bc["dataBase64"].as_str().unwrap().to_string(),
        auth_tag: bc["authTagBase64"].as_str().unwrap().to_string(),
        root_id: bc["rootId"].as_str().unwrap().to_string(),
        nickname: Some(bc["nickname"].as_str().unwrap().to_string()),
        created_at: bc["createdAt"].as_u64().unwrap(),
        updated_at: bc["updatedAt"].as_u64().unwrap(),
    };

    // 1) serde 输出字段集/字段序与设计 §4.2 逐字节一致（字段序由 struct 声明顺序决定）。
    let obj = serde_json::to_value(&compact).unwrap();
    let order: Vec<&str> = obj
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    let expected_order = [
        "v",
        "kdf",
        "salt",
        "iv",
        "data",
        "authTag",
        "rootId",
        "nickname",
        "createdAt",
        "updatedAt",
    ];
    assert_eq!(order, expected_order, "serde 字段序必须与 §4.2 一致");
    // 字段集：不含 publicKeyHex / version / pk。
    assert!(!obj.as_object().unwrap().contains_key("publicKeyHex"));
    assert!(!obj.as_object().unwrap().contains_key("version"));
    assert!(!obj.as_object().unwrap().contains_key("pk"));

    // 2) decode_compact_backup：base64 回 hex + 补 version:2，加密字段与向量一致。
    let file = decode_compact_backup(&compact).unwrap();
    assert_eq!(file.version, 2);
    assert_eq!(file.kdf, "scrypt");
    assert_eq!(file.salt, bc["saltHex"].as_str().unwrap());
    assert_eq!(file.iv, bc["ivHex"].as_str().unwrap());
    assert_eq!(file.data, bc["ciphertextHex"].as_str().unwrap());
    assert_eq!(
        file.auth_tag.as_deref(),
        Some(bc["authTagHex"].as_str().unwrap())
    );
    assert_eq!(file.root_id, bc["rootId"].as_str().unwrap());
    assert_eq!(file.nickname.as_deref(), Some("Vec User"));
    assert_eq!(file.created_at, bc["createdAt"].as_u64().unwrap());
    assert_eq!(file.updated_at, bc["updatedAt"].as_u64().unwrap());

    // 3) 恢复端解锁后以派生公钥补全 publicKeyHex（权威来源）== 期望派生值。
    let mut file = file;
    let (_, identity) = unlock_identity(&file, bc["password"].as_str().unwrap()).unwrap();
    file.public_key_hex = identity.public_key_hex();
    assert_eq!(
        file.public_key_hex,
        bc["expectedDerivedPublicKeyHex"].as_str().unwrap()
    );
    assert_eq!(
        identity.id(),
        bc["rootId"].as_str().unwrap(),
        "派生 identity.id() 必须匹配 rootId 锚点"
    );
}

/// base64 编码（STANDARD，与实现 crypto.rs 同款）。
fn base64_encode(bytes: &[u8]) -> String {
    use base64::engine::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// base64 解码（STANDARD）。
fn base64_decode(s: &str) -> Vec<u8> {
    use base64::engine::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("valid base64")
}

/// v1 身份文件（由向量固定值组装）→ unlock → 迁移 v2 → 再 unlock。
#[test]
fn v1_file_unlock_and_migrate_to_v2() {
    let v = vectors();
    let pv = &v["pbkdf2V1"];
    let rv = &v["rootIdentityEnglishV1"];
    let password = pv["password"].as_str().unwrap();

    // 按 v1 文件布局组装（kdf=pbkdf2，无 authTag）
    let v1_file = IdentityFile {
        version: 1,
        kdf: "pbkdf2".to_string(),
        salt: pv["saltHex"].as_str().unwrap().to_string(),
        iv: pv["ivHex"].as_str().unwrap().to_string(),
        data: pv["ciphertextHex"].as_str().unwrap().to_string(),
        auth_tag: None,
        public_key_hex: rv["publicKeyHex"].as_str().unwrap().to_string(),
        root_id: rv["rootId"].as_str().unwrap().to_string(),
        nickname: Some("  Vec User  ".to_string()),
        avatar: None,
        gender: None,
        region: None,
        signature: None,
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_000,
    };

    // JSON 序列化往返（文件落盘形态）
    let json = v1_file.to_json().unwrap();
    let v1_file = IdentityFile::from_json(&json).unwrap();

    // 解锁 v1：payload 使用 `derivationPath` 字段（真实 TS 落盘格式）
    let (payload, identity) = unlock_identity(&v1_file, password).unwrap();
    assert_eq!(payload.mnemonic, rv["mnemonic"].as_str().unwrap());
    assert_eq!(payload.path, rv["derivationPath"].as_str().unwrap());
    assert_eq!(
        identity.public_key_hex(),
        rv["publicKeyHex"].as_str().unwrap()
    );
    assert_eq!(identity.id(), rv["rootId"].as_str().unwrap());

    // 迁移到 v2
    let v2_file = migrate_v1_to_v2(&v1_file, password).unwrap();
    assert_eq!(v2_file.version, 2);
    assert_eq!(v2_file.kdf, "scrypt");
    assert!(v2_file.auth_tag.is_some());
    assert_eq!(v2_file.iv.len(), 24); // 12 字节 hex
    assert_eq!(v2_file.root_id, v1_file.root_id);
    assert_eq!(v2_file.public_key_hex, v1_file.public_key_hex);
    assert_eq!(v2_file.created_at, v1_file.created_at);
    assert_eq!(v2_file.nickname.as_deref(), Some("Vec User")); // sanitize 去空格

    // v2 再解锁，内容一致
    let (payload2, identity2) = unlock_identity(&v2_file, password).unwrap();
    assert_eq!(payload2.mnemonic, payload.mnemonic);
    assert_eq!(payload2.path, payload.path);
    assert_eq!(identity2.public_key_hex(), identity.public_key_hex());
    // 资料已移出 payload：昵称只在明文头
    assert_eq!(v2_file.nickname.as_deref(), Some("Vec User"));
}

#[test]
fn update_profile_flow() {
    use spark_core::identity::file::{create_identity, update_profile};

    let (mut file, identity) =
        create_identity("P@ssw0rd-test", "初始昵称", None).expect("create identity");
    assert_eq!(file.nickname.as_deref(), Some("初始昵称"));
    assert_eq!(file.avatar, None);

    // 修改昵称 + 设置头像
    update_profile(
        &mut file,
        "P@ssw0rd-test",
        Some("新昵称"),
        Some(Some("data:image/png;base64,iVBORw0KGgo=")),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(file.nickname.as_deref(), Some("新昵称"));
    assert_eq!(
        file.avatar.as_deref(),
        Some("data:image/png;base64,iVBORw0KGgo=")
    );
    assert!(file.updated_at >= file.created_at);

    // 解锁正常（payload 只含 mnemonic/path），昵称以明文头为准
    let (_payload, unlocked) = unlock_identity(&file, "P@ssw0rd-test").unwrap();
    assert_eq!(file.nickname.as_deref(), Some("新昵称"));
    assert_eq!(unlocked.public_key_hex(), identity.public_key_hex());

    // 清除头像（Some(None)），昵称不变（None）
    update_profile(
        &mut file,
        "P@ssw0rd-test",
        None,
        Some(None),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(file.avatar, None);
    assert_eq!(file.nickname.as_deref(), Some("新昵称"));

    // 非法昵称/头像被拒
    assert!(
        update_profile(
            &mut file,
            "P@ssw0rd-test",
            Some(&"x".repeat(25)),
            None,
            None,
            None,
            None
        )
        .is_err()
    );
    assert!(
        update_profile(
            &mut file,
            "P@ssw0rd-test",
            None,
            Some(Some("http://a.png")),
            None,
            None,
            None
        )
        .is_err()
    );

    // 错误密码不能解锁
    assert!(unlock_identity(&file, "bad-password").is_err());
}

#[test]
fn generated_mnemonic_is_valid_chinese() {
    use spark_core::identity::mnemonic::generate_mnemonic;

    let m = generate_mnemonic().unwrap();
    assert_eq!(m.split_whitespace().count(), 24);
    let parsed = parse_mnemonic(&m).unwrap();
    assert_eq!(parsed.wordlist, Wordlist::ChineseSimplified);

    // 英文助记词不会被误判为中文
    let en = "wage secret force quantum hurt village fire success duck leader virus off flip possible ethics muscle actual cannon ritual express often wall excess room";
    assert_eq!(parse_mnemonic(en).unwrap().wordlist, Wordlist::English);

    // 垃圾输入被拒
    assert!(parse_mnemonic("foo bar baz").is_err());
    assert!(parse_mnemonic("").is_err());
}

#[test]
fn nickname_boundary_via_public_api() {
    assert!(validate_nickname(" 甲 ").is_ok());
    assert_eq!(validate_nickname(" 甲 ").unwrap(), "甲");
}

fn sha2_sm(domain: &str) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(domain.as_bytes()).into()
}

/// 路径格式化（补充断言 domain 路径拼接逻辑）。
#[test]
fn path_formatting() {
    let indices = [44, 607, 0, 0, 0, 836792189, 167688602];
    assert_eq!(
        format_derivation_path(&indices),
        "m/44'/607'/0'/0'/0'/836792189'/167688602'"
    );
}
