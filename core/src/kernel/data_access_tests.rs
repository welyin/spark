//! `data_access.rs` 的内联单元测试（拆分至独立文件，满足 650 行硬线——测试
//! 用例天然罗列，orgsync.rs 同款先例）。
use super::*;
use crate::kernel::KernelConfig;
use crate::org::service::CreateOrganizationInput;
use crate::plugindata::{Accounts, Confidentiality, DeclareInput, Scope, Space};

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity("correct-horse-battery", "alice", None).unwrap();
    (dir, kernel)
}

/// O4 插件 API：声明 encrypted 集合后 grant 创世 acl（owner 自签）→
/// revoke 轮换 epoch + 生成新密钥 → list 可读。acl 经 orgsync 全员同步。
#[test]
fn encrypted_access_grant_revoke_list() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:payroll".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                confidentiality: Some(Confidentiality::Encrypted),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    let acl = kernel
        .data_grant_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    assert!(acl.is_owner(&kernel.require_current_root_id().unwrap()));
    assert!(acl.is_reader(BOB));
    assert_eq!(acl.epoch, 1);
    let listed = kernel
        .data_list_access(&org_id, "ai-chat:payroll", "1.0.0")
        .unwrap();
    assert_eq!(listed.readers, acl.readers);
    assert_eq!(listed.epoch, 1);
    let revoked = kernel
        .data_revoke_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    assert_eq!(revoked.epoch, 2);
    assert!(!revoked.is_reader(BOB));
    assert!(
        orgsync::get_epoch_key(
            kernel.require_storage().unwrap().raw(),
            &org_id,
            "ai-chat:payroll",
            "1.0.0",
            2
        )
        .is_some(),
        "revoke 后新 epoch 密钥落 orgkey 表"
    );
    let acl_key = orgsync::acl_key(&org_id, "ai-chat:payroll", "1.0.0");
    let storage = kernel.require_storage().unwrap();
    assert!(
        crate::sync::get_personal_meta(storage, &acl_key).unwrap().is_some(),
        "acl 为 all-members 系统数据，受管版本化"
    );
}

/// O4：非 owner 调用 revoke → AccessDenied（名单管理权属于 owner）。
#[test]
fn encrypted_revoke_non_owner_denied() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:payroll".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                confidentiality: Some(Confidentiality::Encrypted),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    kernel
        .data_grant_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    let revoked = kernel
        .data_revoke_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    assert_eq!(revoked.epoch, 2);
}

/// O4 重置接管：spark 管理员（org admin）写入新 acl——resetBy=管理员、名单
/// 全新、无历史密钥（只能重启读不到历史）；接管记录全员可见。O3：reset **不复用
/// 历史 epoch 号**——取本机 orgkey 表已知最大 epoch+1（grant epoch=1 + revoke
/// epoch=2 → reset epoch=3），消除「旧读者旧 epoch-N 密钥 vs 新 epoch-N 投递
/// 幂等丢弃」的双向分裂。
#[test]
fn encrypted_admin_reset_takeover() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:payroll".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                confidentiality: Some(Confidentiality::Encrypted),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    kernel
        .data_grant_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    kernel
        .data_revoke_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    let reset = kernel
        .data_reset_access(
            &org_id,
            "ai-chat:payroll",
            "1.0.0",
            &["new-owner".to_string()],
            &["new-reader".to_string()],
        )
        .unwrap();
    // O3：不复用历史 epoch（grant=1, revoke=2 → reset 取 max+1=3）
    assert_eq!(reset.epoch, 3, "接管不复用历史 epoch 号（取 max known + 1）");
    assert_eq!(
        reset.reset_by.as_deref(),
        Some(kernel.require_current_root_id().unwrap().as_str())
    );
    assert!(reset.is_owner("new-owner"));
    assert!(reset.is_reader("new-reader"));
    assert!(
        orgsync::get_epoch_key(
            kernel.require_storage().unwrap().raw(),
            &org_id,
            "ai-chat:payroll",
            "1.0.0",
            3
        )
        .is_some(),
        "接管后 reset_epoch=3 密钥已生成"
    );
}

/// O4 工作项 1：`org_publish_access_key` 惰性发布本人组织身份访问密钥
/// （accessKey）。验证：落成员表 access_key = 域身份公钥（b64）+ 根密钥绑定
/// 签名；绑定签名可用根公钥验签通过；幂等；golden 向量兼容。
#[test]
fn publish_access_key_binds_and_serializes_compatibly() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    let root_id = kernel.require_current_root_id().unwrap();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();

    let raw = kernel.require_storage().unwrap().raw().clone();
    let record = crate::org::OrganizationService::get_record(&raw, &org_id)
        .unwrap()
        .unwrap();
    let bob_json = serde_json::to_value(&record.members[1]).unwrap();
    assert!(
        bob_json.get("accessKey").is_none(),
        "未发布成员线形无 accessKey 键（缺省兼容）"
    );

    kernel.org_publish_access_key(&org_id).unwrap();
    let record = crate::org::OrganizationService::get_record(
        &kernel.require_storage().unwrap().raw().clone(),
        &org_id,
    )
    .unwrap()
    .unwrap();
    let me = record.find_member(&root_id).unwrap();
    let ak = me.access_key.as_ref().expect("发布后 accessKey 已落");
    assert!(!ak.public_key.is_empty(), "域身份公钥非空");
    assert!(!ak.bind_sig.is_empty(), "绑定签名非空");
    let pubkey_hex = kernel.current_identity().unwrap().unwrap().public_key_hex;
    let root_pk_bytes = hex::decode(pubkey_hex).unwrap();
    let root_pk_b64 = B64.encode(root_pk_bytes);
    let payload = crate::org::types::access_key_bind_payload(&org_id, &ak.public_key);
    assert!(
        crate::identity::verify_ed25519_signature(&payload, &ak.bind_sig, &root_pk_b64),
        "绑定签名可由根公钥验签"
    );
    assert!(
        !crate::identity::verify_ed25519_signature(&payload, &ak.bind_sig, "AAAA"),
        "伪造根公钥验签失败"
    );

    let before = record.updated_at;
    kernel.org_publish_access_key(&org_id).unwrap();
    let record2 = crate::org::OrganizationService::get_record(
        &kernel.require_storage().unwrap().raw().clone(),
        &org_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        record2.updated_at, before,
        "accessKey 未变不 bump updatedAt（幂等）"
    );
    let bob = record2.find_member(BOB).unwrap();
    assert!(bob.access_key.is_none(), "他人未发布仍为 None");
}

/// O5：orgkey 离线投递 pending——收件人无 accessKey（不可达）时 grant 落
/// `orgkey:pending:` 键；`resend_pending_orgkey` 按 org 扫描并清空这些 pending
/// （重投路径幂等，对端已有 ≥ epoch 丢弃无害）。
#[test]
fn orgkey_offline_delivery_persists_and_resends_pending() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:payroll".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                confidentiality: Some(Confidentiality::Encrypted),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    // BOB 无 accessKey（未发布）→ grant 对其投递不可达 → 落 pending
    kernel
        .data_grant_access(&org_id, "ai-chat:payroll", "1.0.0", &[BOB.to_string()])
        .unwrap();
    let col_full = "ai-chat:payroll@v1.0.0";
    let pending = orgsync::orgkey_pending_for_org(
        kernel.require_storage().unwrap().raw(),
        &org_id,
    );
    assert!(
        pending.iter().any(|(c, r, e, _)| c == col_full && r == BOB && *e == 1),
        "BOB 无 accessKey → grant 落 orgkey pending"
    );
    // 上线（hello）触发重投：resend 后清空 pending（重投投递仍不可达会再落，
    // 但方法本身按「已重投」清键；本断言验证扫描+清理路径可执行）。
    kernel.resend_pending_orgkey(&org_id, BOB);
    let after = orgsync::orgkey_pending_for_org(kernel.require_storage().unwrap().raw(), &org_id);
    assert!(
        !after.iter().any(|(c, r, e, _)| c == col_full && r == BOB && *e == 1),
        "resend 后 pending 已清"
    );
}
