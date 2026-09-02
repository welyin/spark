//! `access.rs` 的内联单元测试（拆分至独立文件，满足 650 行硬线——access.rs 的
//! 实现部分约 590 行，测试天然罗列另置；与 data_access_tests.rs 同款先例）。

use super::*;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn key() -> [u8; 32] {
    [7u8; 32]
}

#[test]
fn encrypt_decrypt_roundtrip() {
    let k = key();
    let enc = encrypt_value("org_x", "fin:pay@v1.0.0", "2026-08", 1, &k, r#"{"a":1}"#)
        .unwrap();
    assert_eq!(enc["epoch"], json!(1));
    assert!(enc["nonce"].is_string());
    assert!(enc["ct"].is_string());
    let dec = decrypt_value("org_x", "fin:pay@v1.0.0", "2026-08", &k, &enc).unwrap();
    assert_eq!(dec, r#"{"a":1}"#);
}

#[test]
fn decrypt_wrong_key_fails() {
    let enc = encrypt_value("o", "c@v1", "k", 1, &key(), "v").unwrap();
    let wrong = [9u8; 32];
    assert!(decrypt_value("o", "c@v1", "k", &wrong, &enc).is_none());
}

#[test]
fn decrypt_wrong_aad_fails() {
    let enc = encrypt_value("o", "c@v1", "k", 1, &key(), "v").unwrap();
    // 换集合/换 key → AAD 不匹配 → 解密失败（防跨记录/跨集合搬迁）
    assert!(decrypt_value("o", "c@v1", "other", &key(), &enc).is_none());
    assert!(decrypt_value("o", "other@v1", "k", &key(), &enc).is_none());
    assert!(decrypt_value("other", "c@v1", "k", &key(), &enc).is_none());
}

#[test]
fn acl_keys() {
    assert_eq!(
        orgkey_key("org_01", "fin:pay", "1", 3),
        "orgkey:org_01:fin:pay@v1:3"
    );
    assert_eq!(
        acl_key("org_01", "fin:pay", "1"),
        "org:acl:org_01:fin:pay@v1"
    );
    assert_eq!(
        parse_acl_key("org:acl:org_01:fin:pay@v1"),
        Some(("org_01".to_string(), "fin:pay".to_string(), "1".to_string()))
    );
    assert!(parse_acl_key("orgd:org_01:fin:pay@v1:k").is_none());
}

#[test]
fn acl_sign_verify() {
    let sk = SigningKey::from_bytes(&[1u8; 32]);
    let pk = sk.verifying_key();
    let mut acl = AclRecord {
        owners: vec!["owner1".to_string()],
        readers: vec!["reader1".to_string()],
        epoch: 1,
        updated_at: 100,
        reset_by: None,
        sig: String::new(),
    };
    let payload = acl_sign_payload(
        acl.epoch,
        "org_01",
        "fin:pay@v1",
        &acl.owners,
        &acl.readers,
        None,
        acl.updated_at,
    );
    acl.sig = acl_sign(&sk, &payload);
    assert!(acl_verify(&acl, "org_01", "fin:pay@v1", &pk));
    // 篡改 readers → 验签失败
    let mut tampered = acl.clone();
    tampered.readers = vec!["evil".to_string()];
    assert!(!acl_verify(&tampered, "org_01", "fin:pay@v1", &pk));
}

#[test]
fn acl_merge_updated_at_wins() {
    let cur = AclRecord {
        owners: vec!["o".into()],
        readers: vec!["a".into()],
        epoch: 1,
        updated_at: 100,
        reset_by: None,
        sig: "".into(),
    };
    let newer = AclRecord {
        owners: vec!["o".into()],
        readers: vec!["a".into(), "b".into()],
        epoch: 2,
        updated_at: 200,
        reset_by: None,
        sig: "".into(),
    };
    let older = AclRecord {
        epoch: 5,
        updated_at: 50,
        ..newer.clone()
    };
    assert_eq!(acl_merge(&cur, &newer).epoch, 2, "大者胜");
    assert_eq!(acl_merge(&cur, &older).epoch, 1, "旧者保留本地");
}

#[test]
fn epoch_key_roundtrip() {
    let mut store = crate::storage::MemoryStorage::new();
    let k = [3u8; 32];
    put_epoch_key(&mut store, "org_01", "fin:pay", "1", 2, &k);
    assert_eq!(get_epoch_key(&store, "org_01", "fin:pay", "1", 2).unwrap(), k);
    assert!(get_epoch_key(&store, "org_01", "fin:pay", "1", 3).is_none());
}

#[test]
fn box_unbox_roundtrip() {
    let owner_sk = SigningKey::from_bytes(&[5u8; 32]);
    let recipient_sk = SigningKey::from_bytes(&[6u8; 32]);
    let owner_x = ed_sk_to_x25519(&owner_sk.to_bytes());
    let recipient_x = ed_sk_to_x25519(&recipient_sk.to_bytes());
    let recipient_pub = ed_pk_to_x25519(&recipient_sk.verifying_key().to_bytes()).unwrap();
    let owner_pub = ed_pk_to_x25519(&owner_sk.verifying_key().to_bytes()).unwrap();
    let epoch_key = [9u8; 32];
    let ctx = ("org_01", "fin:pay@v1", "owner-root", "reader-root");
    let (wrapped, nonce24) =
        box_epoch_key(&epoch_key, &recipient_pub, &owner_x, ctx.0, ctx.1, ctx.2, ctx.3)
            .unwrap();
    let got = unbox_epoch_key(&wrapped, &nonce24, &owner_pub, &recipient_x, ctx.0, ctx.1, ctx.2, ctx.3)
        .unwrap();
    assert_eq!(got, epoch_key);
    // 第三方解不开（用另一个私钥）
    let evil_x = ed_sk_to_x25519(&[0u8; 32]);
    assert!(
        unbox_epoch_key(&wrapped, &nonce24, &owner_pub, &evil_x, ctx.0, ctx.1, ctx.2, ctx.3)
            .is_none()
    );
    // H1b：域分隔——换集合上下文后 unbox 失败（同一 DH 共享不跨上下文复用）
    assert!(
        unbox_epoch_key(&wrapped, &nonce24, &owner_pub, &recipient_x, ctx.0, "other@v1", ctx.2, ctx.3)
            .is_none(),
        "域分隔：换 collection 上下文解不开"
    );
}

#[test]
fn is_reader_is_owner() {
    let acl = AclRecord {
        owners: vec!["o".into()],
        readers: vec!["r".into()],
        epoch: 1,
        updated_at: 0,
        reset_by: None,
        sig: "".into(),
    };
    assert!(acl.is_reader("r"));
    assert!(!acl.is_reader("x"));
    assert!(acl.is_owner("o"));
    assert!(!acl.is_owner("r"));
}

/// O4 工作项 3：orgkey-deliver 出站 body 构建 + 接收侧解包端到端——
/// owner 用 recipient 组织身份公钥 X25519 包裹 epoch 密钥，recipient 用
/// 自己组织身份私钥 X25519 解包得到原密钥（crypto_box DH 共享密钥一致）。
/// 第三方解不开。
#[test]
fn orgkey_deliver_build_and_unbox_roundtrip() {
    let owner_sk = SigningKey::from_bytes(&[11u8; 32]);
    let recipient_sk = SigningKey::from_bytes(&[12u8; 32]);
    let owner_x25519 = ed_sk_to_x25519(&owner_sk.to_bytes());
    let recipient_x25519 = ed_sk_to_x25519(&recipient_sk.to_bytes());
    let recipient_pub = ed_pk_to_x25519(&recipient_sk.verifying_key().to_bytes()).unwrap();
    let epoch_key = [5u8; 32];
    // 出站构建
    let body = build_orgkey_deliver(
        "org_01",
        "fin:pay",
        "1",
        3,
        &epoch_key,
        "owner-root-id",
        "reader-root-id",
        &recipient_pub,
        &owner_sk,
        &owner_x25519,
        1234,
    )
    .expect("build deliver");
    assert_eq!(body["orgId"], json!("org_01"));
    assert_eq!(body["collection"], json!("fin:pay@v1"));
    assert_eq!(body["epoch"], json!(3));
    assert_eq!(body["recipientRootId"], json!("reader-root-id"));
    assert!(body["wrappedKey"].is_string());
    assert!(body["nonce"].is_string());
    assert!(body["sig"].is_string());
    // 接收侧解包（sender=owner 组织身份公钥 X25519）；H1b 域分隔上下文
    // = (orgId, collection, senderRootId, recipientRootId)
    let sender_pub = ed_pk_to_x25519(&owner_sk.verifying_key().to_bytes()).unwrap();
    let got = unbox_epoch_key(
        body["wrappedKey"].as_str().unwrap(),
        body["nonce"].as_str().unwrap(),
        &sender_pub,
        &recipient_x25519,
        "org_01",
        "fin:pay@v1",
        "owner-root-id",
        "reader-root-id",
    )
    .unwrap();
    assert_eq!(got, epoch_key, "recipient 解包得原 epoch 密钥");
    // H1c：域分隔跨对端拒绝——用错误 recipientRootId 上下文解不开
    assert!(
        unbox_epoch_key(
            body["wrappedKey"].as_str().unwrap(),
            body["nonce"].as_str().unwrap(),
            &sender_pub,
            &recipient_x25519,
            "org_01",
            "fin:pay@v1",
            "owner-root-id",
            "wrong-recipient",
        )
        .is_none(),
        "域分隔：换 recipient 上下文解不开（防转投）"
    );
    // 第三方（evil 私钥）解不开
    let evil_x = ed_sk_to_x25519(&[0u8; 32]);
    assert!(
        unbox_epoch_key(
            body["wrappedKey"].as_str().unwrap(),
            body["nonce"].as_str().unwrap(),
            &sender_pub,
            &evil_x,
            "org_01",
            "fin:pay@v1",
            "owner-root-id",
            "reader-root-id",
        )
        .is_none(),
        "第三方解不开 orgkey-deliver"
    );
    // body 可反序列化为 OrgkeyDeliver 线形（接收侧 parse）
    let parsed = parse_orgkey_deliver(&body).expect("parse deliver");
    assert_eq!(parsed.epoch, 3);
    assert_eq!(parsed.collection, "fin:pay@v1");
    assert_eq!(parsed.recipient_root_id, "reader-root-id");
}

/// O5：orgkey pending 键构造 + 扫描解析（含 collection 含 `:` 插件前缀分隔符
/// 的右向解析）——`orgkey_pending_for_org` 按 org 扫描返回 (collection,
/// recipientRootId, epoch, ts)。
#[test]
fn orgkey_pending_key_and_scan_parse() {
    let mut store = crate::storage::MemoryStorage::new();
    let org = "org_0000000000000001";
    let col = "ai-chat:payroll@v1.0.0"; // collection 含 `:`（插件前缀分隔符）
    let recipient = "b".repeat(64);
    orgkey_pending_put(&mut store, org, col, &recipient, 3, 1700);
    let pendings = orgkey_pending_for_org(&store, org);
    assert_eq!(
        pendings,
        vec![(col.to_string(), recipient.clone(), 3, 1700)],
        "pending 解析（右向）正确还原 collection/recipient/epoch/ts"
    );
    // 换 org 扫描不到
    assert!(orgkey_pending_for_org(&store, "org_other").is_empty());
    // 删除后消失
    orgkey_pending_remove(&mut store, org, col, &recipient, 3);
    assert!(orgkey_pending_for_org(&store, org).is_empty());
}

/// F3 残余 §7.2 防回归守卫：pending 键迁出同步命名空间——`orgkey-pending:`
/// 连字符前缀不匹配 pdsync `orgkey:` category、不属任何 orgsync 受管键域；
/// 版本化句柄写 pending 不产生 pmeta（本地键不进同步流量，任何句柄都不会
/// 误入——命名空间级不变量，不依赖调用方走 raw）。
#[test]
fn orgkey_pending_key_outside_sync_namespace() {
    let key = orgkey_pending_key("org_01", "ai-chat:payroll@v1.0.0", &"r".repeat(64), 1);
    assert!(key.starts_with("orgkey-pending:"));
    assert!(
        crate::sync::pdsync::category_for_key(&key).is_none(),
        "pending 键不匹配任何 pdsync category"
    );
    assert!(
        crate::sync::orgsync::legacy_org_key_scope(&key).is_none(),
        "pending 键不属内建集合键域"
    );
    // 版本化句柄写 pending：不产生 pmeta（修复前 orgkey: 前缀误入 category）
    use crate::storage::StorageBackend as _;
    let mut s = crate::sync::versioned::VersionedStorage::new(
        crate::storage::MemoryStorage::new(),
        crate::sync::versioned::shared_node_id("node-a"),
    );
    s.put(&key, "1700").unwrap();
    assert!(
        crate::sync::get_personal_meta(s.raw(), &key).unwrap().is_none(),
        "版本化句柄写 pending 不产生 pmeta"
    );
    // 重投删除后不残留（无孤儿 pmeta 可留——从未产生）
    s.delete(&key).unwrap();
    assert!(s.get(&key).unwrap().is_none());
    assert!(
        crate::sync::get_personal_meta(s.raw(), &key).unwrap().is_none(),
        "删除后无孤儿 pmeta"
    );
    // 暂存键同口径（§7.1 本地键）
    let stash = orgkey_stash_key("org_01", "ai-chat:payroll@v1.0.0", &"s".repeat(64), 2);
    assert!(crate::sync::pdsync::category_for_key(&stash).is_none());
}

/// F3 残余 §7.1：暂存键 put/scan/remove 往返 + 同键 ts 新者覆盖（去重）。
#[test]
fn orgkey_stash_roundtrip_and_ts_overwrite() {
    let mut store = crate::storage::MemoryStorage::new();
    let org = "org_0000000000000001";
    let col = "ai-chat:payroll@v1.0.0"; // 含 `:` 分隔符（右向解析）
    let sender = "s".repeat(64);
    let body_old = json!({"orgId": org, "collection": col, "epoch": 2, "ts": 1000});
    let body_new = json!({"orgId": org, "collection": col, "epoch": 2, "ts": 2000});
    orgkey_stash_put(&mut store, org, col, &sender, 2, &body_old);
    orgkey_stash_put(&mut store, org, col, &sender, 2, &body_new);
    let stashed = orgkey_stash_for_org(&store, org);
    assert_eq!(stashed.len(), 1, "同键去重");
    assert_eq!(stashed[0].0, col);
    assert_eq!(stashed[0].1, sender);
    assert_eq!(stashed[0].2, 2);
    assert_eq!(stashed[0].3, serde_json::to_string(&body_new).unwrap(), "ts 新者覆盖");
    // 旧 ts 不回覆盖
    orgkey_stash_put(&mut store, org, col, &sender, 2, &body_old);
    assert_eq!(
        orgkey_stash_for_org(&store, org)[0].3,
        serde_json::to_string(&body_new).unwrap(),
        "旧 ts 不覆盖新者"
    );
    // 换 org 扫描不到；删除后消失
    assert!(orgkey_stash_for_org(&store, "org_other").is_empty());
    orgkey_stash_remove(&mut store, org, col, &sender, 2);
    assert!(orgkey_stash_for_org(&store, org).is_empty());
    // 暂存不污染同步流量：无 pmeta 产生（裸写路径）
    assert!(
        crate::sync::get_personal_meta(&store, &orgkey_stash_key(org, col, &sender, 2))
            .unwrap()
            .is_none()
    );
}
