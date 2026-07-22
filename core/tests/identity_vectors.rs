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
use spark_core::identity::file::{
    IdentityFile, migrate_v1_to_v2, unlock_identity, validate_nickname,
};
use spark_core::identity::derive::{derive_domain_identity, derive_root_identity, domain_indices};
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
        assert_eq!(h, dv["domainSha256Hex"].as_str().unwrap(), "sha256({domain})");
        let (idx_a, idx_b) = domain_indices(domain);
        assert_eq!(idx_a, dv["idxA"].as_u64().unwrap() as u32, "idxA({domain})");
        assert_eq!(idx_b, dv["idxB"].as_u64().unwrap() as u32, "idxB({domain})");

        // 完整路径
        let identity = derive_domain_identity(&parsed.seed, domain);
        assert_eq!(identity.path, dv["derivationPath"].as_str().unwrap());
        assert_eq!(identity.public_key_hex(), dv["publicKeyHex"].as_str().unwrap());
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
    assert_eq!(identity.public_key_hex(), rv["publicKeyHex"].as_str().unwrap());
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
    assert_eq!(payload2.nickname.as_deref(), Some("Vec User"));
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
    )
    .unwrap();
    assert_eq!(file.nickname.as_deref(), Some("新昵称"));
    assert_eq!(
        file.avatar.as_deref(),
        Some("data:image/png;base64,iVBORw0KGgo=")
    );
    assert!(file.updated_at >= file.created_at);

    // 解锁后 payload 同步
    let (payload, unlocked) = unlock_identity(&file, "P@ssw0rd-test").unwrap();
    assert_eq!(payload.nickname.as_deref(), Some("新昵称"));
    assert_eq!(unlocked.public_key_hex(), identity.public_key_hex());

    // 清除头像（Some(None)），昵称不变（None）
    update_profile(&mut file, "P@ssw0rd-test", None, Some(None)).unwrap();
    assert_eq!(file.avatar, None);
    assert_eq!(file.nickname.as_deref(), Some("新昵称"));

    // 非法昵称/头像被拒
    assert!(
        update_profile(&mut file, "P@ssw0rd-test", Some(&"x".repeat(25)), None).is_err()
    );
    assert!(
        update_profile(&mut file, "P@ssw0rd-test", None, Some(Some("http://a.png"))).is_err()
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
