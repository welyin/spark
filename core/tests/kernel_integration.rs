//! kernel 门面集成测试：tempdir + sled，覆盖身份全流程、doc 写入路径、
//! 组织/邀请码、purge 流程与 p2p 起停。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use spark_core::collection::{CollectionConfig, QueryOptions};
use spark_core::kernel::{Kernel, KernelConfig, KernelError};
use spark_core::org::invite::{OrgInviteInviter, OrgInvitePayload, encode_org_invite};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::node::system_now_ms;
use spark_core::p2p::{P2pConfig, P2pEvent};
use spark_core::schema::{CollectionSchemaDeclaration, SyncStrategy};

const PASSWORD: &str = "password123";

fn test_p2p_config() -> P2pConfig {
    P2pConfig {
        app_version: "0.0.0-test".to_string(),
        preferred_port: Some(0),
        port_scan: false,
        enable_tcp: true,
        enable_ws: false,
        enable_ipv6: false,
        enable_mdns: false,
        enable_upnp: false,
        keepalive_interval: None,
        now_fn: Arc::new(system_now_ms),
    }
}

fn config(dir: &Path) -> KernelConfig {
    KernelConfig {
        data_dir: dir.to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: Some(test_p2p_config()),
    }
}

fn fresh_kernel(dir: &Path) -> Kernel {
    Kernel::init(config(dir)).expect("kernel init")
}

fn init_identity(kernel: &mut Kernel) -> (String, String) {
    let result = kernel
        .init_identity(PASSWORD, "  小明  ", None)
        .expect("init identity");
    assert_eq!(result.root_id.len(), 64, "rootId 为 64 字符 hex");
    (result.root_id, result.mnemonic)
}

fn lww_evidence_declaration() -> CollectionSchemaDeclaration {
    CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence: true,
    }
}

fn from_indexed_config() -> CollectionConfig {
    CollectionConfig {
        indexed_fields: vec!["from".to_string()],
        ..Default::default()
    }
}

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
    let identity_path = dir.path().join("identities").join(format!("{root_id}.json"));
    assert!(identity_path.exists());
    let active_raw = std::fs::read_to_string(dir.path().join("active-identity.json")).unwrap();
    assert_eq!(active_raw, format!(r#"{{"activeRootId":"{root_id}"}}"#), "活动指针逐字节对齐 TS");

    // 身份文件：两空格缩进（JSON.stringify(payload, null, 2) 风格）、v2 字段齐备
    let file_raw = std::fs::read_to_string(&identity_path).unwrap();
    assert!(file_raw.starts_with("{\n  \"version\": 2,"), "两空格缩进、version 为首字段");
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
    let profile = kernel
        .update_profile(PASSWORD, Some("小红"), None)
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小红"));
    assert_eq!(kernel.status().unwrap().nickname.as_deref(), Some("小红"));
    let err = kernel.update_profile("wrong-password", Some("x"), None).unwrap_err();
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
    let err = kernel_b.recover_backup(&backup, "wrong-password").unwrap_err();
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
    assert_eq!(err.to_string(), "助记词校验失败：请检查是否有错别字、漏字或顺序错误");
    kernel_c.shutdown().unwrap();

    // 设备 A：同一助记词恢复 → 拒绝（已在本设备）
    let err = kernel_a
        .recover_mnemonic(&mnemonic, PASSWORD, "x", None)
        .unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    kernel_a.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// doc 写入路径：meta/evidence/索引、append-only 拒绝、重启持久化
// ---------------------------------------------------------------------------

#[test]
fn doc_write_meta_evidence_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 声明 lww + 存证集合
    kernel
        .declare_collection("chat", "messages", lww_evidence_declaration())
        .unwrap();

    // 写文档
    kernel
        .doc_put("chat", "messages", "id1", json!({"from": "alice", "text": "hello"}), from_indexed_config())
        .unwrap();
    let doc = kernel.doc_get("chat", "messages", "id1").unwrap().unwrap();
    assert_eq!(doc, json!({"from": "alice", "text": "hello"}));

    // lww 覆盖 + 索引查询
    kernel
        .doc_put("chat", "messages", "id1", json!({"from": "bob", "text": "hi"}), from_indexed_config())
        .unwrap();
    let result = kernel
        .doc_query(
            "chat",
            "messages",
            from_indexed_config(),
            QueryOptions {
                index_name: Some("from".to_string()),
                index_value: Some(json!("bob")),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].id, "id1");

    // 导出全库：逐字节检查 doc/meta key 与存证链
    let export_path = dir.path().join("dump.json");
    let written = kernel.export_dump(&export_path).unwrap();
    assert!(written.entries > 0 && written.bytes > 0);
    let dump: Value = serde_json::from_str(&std::fs::read_to_string(&export_path).unwrap()).unwrap();
    let entries: std::collections::HashMap<_, _> = dump["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["key"].as_str().unwrap().to_string(), e["value"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        entries["doc:chat:messages:id1"],
        r#"{"from":"bob","text":"hi"}"#
    );
    let meta_raw = &entries["meta:chat:messages:id1"];
    let meta: Value = serde_json::from_str(meta_raw).unwrap();
    assert_eq!(meta["vv"]["local-node"], 2, "本节点两次写入计数");
    assert_eq!(meta["nodeId"], "local-node");
    assert!(meta_raw.starts_with(r#"{"vv":"#), "meta 键序 vv 在前");
    assert!(
        entries.keys().any(|k| k.starts_with("doc:evidence:proof:")),
        "存证条目已落库"
    );
    assert!(entries.contains_key("doc:evidence:head"));

    // append-only 默认策略拒绝覆盖
    kernel
        .doc_put("chat", "audit", "a1", json!({"v": 1}), CollectionConfig::default())
        .unwrap();
    let err = kernel
        .doc_put("chat", "audit", "a1", json!({"v": 2}), CollectionConfig::default())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Collection \"audit\" is append-only: document \"a1\" already exists and cannot be overwritten"
    );

    // 删除：墓碑 + 返回值语义
    assert!(kernel.doc_delete("chat", "messages", "id1", from_indexed_config()).unwrap());
    assert!(kernel.doc_get("chat", "messages", "id1").unwrap().is_none());
    assert!(!kernel.doc_delete("chat", "messages", "id1", from_indexed_config()).unwrap());
    let entries_after: Value = {
        let p = dir.path().join("dump2.json");
        kernel.export_dump(&p).unwrap();
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let tombstone = entries_after["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "meta:chat:messages:id1")
        .expect("tombstone meta");
    let tombstone: Value = serde_json::from_str(tombstone["value"].as_str().unwrap()).unwrap();
    assert_eq!(tombstone["tombstone"], true);
    assert!(tombstone.get("nodeId").is_none(), "墓碑 meta 不带 nodeId");

    kernel.shutdown().unwrap();

    // 重启：数据仍在（audit 集合的 a1 未被删除）
    let mut kernel = fresh_kernel(dir.path());
    kernel.unlock(PASSWORD, None).unwrap();
    let doc = kernel.doc_get("chat", "audit", "a1").unwrap().unwrap();
    assert_eq!(doc, json!({"v": 1}));
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 组织：创建/列表/副本概览/邀请码（自邀拒绝 + 纯逻辑接受）
// ---------------------------------------------------------------------------

#[test]
fn org_create_invite_and_overview() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);

    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "  测试组织  ".to_string(),
            description: Some("描述".to_string()),
            base_plugin_domain: "plugin:notes".to_string(),
        })
        .unwrap();
    assert_eq!(view.record.name, "测试组织", "组织名 trim");
    assert!(view.is_current_user_admin);
    assert_eq!(view.member_count, 1);
    let org_id = view.record.org_id.clone();

    assert_eq!(kernel.list_orgs().unwrap().len(), 1);

    // 副本概览：本机恒算 1 个副本
    let overview = kernel.org_overview(&org_id).unwrap();
    assert_eq!(overview.replica_target, 3);
    assert_eq!(overview.synced_peers, 1);
    assert_eq!(overview.members.len(), 1);
    assert!(overview.members[0].is_self && overview.members[0].ever_synced);

    // 未启动 p2p 生成邀请码 → 网络不可用
    let err = kernel.create_org_invite(&org_id).unwrap_err();
    assert_eq!(err.to_string(), "本机 P2P 节点尚未启动，请先启动网络后再生成邀请码");

    // 自邀拒绝（直接用 org 模块构造邀请码，纯逻辑路径；inviter 需带节点信息
    // 否则解码阶段先报"缺少邀请人的节点地址"）
    let self_code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "测试组织".to_string(),
        OrgInviteInviter {
            root_id: root_id.clone(),
            peer_id: Some("self-peer-123456".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    let err = kernel.join_by_invite(&self_code).unwrap_err();
    assert_eq!(err.to_string(), "不能接受自己发出的邀请码");

    // 他人邀请码：解码通过（后续连接拉取由壳层完成）
    let other_root = "ab".repeat(32);
    let other_code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "测试组织".to_string(),
        OrgInviteInviter {
            root_id: other_root.clone(),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    let payload = kernel.join_by_invite(&other_code).unwrap();
    assert_eq!(payload.org_id, org_id);
    assert_eq!(payload.inviter.root_id, other_root);
    // 尚未拉取到成员记录 → 确认加入失败（用本机不存在的组织 id 模拟拉取后仍非成员）
    let err = kernel.check_join("org_nonexistent").unwrap_err();
    assert_eq!(err.to_string(), "未能加入组织：请确认管理员已先将你的 RootID 录入组织成员");

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// purge：预览 + 校验顺序（管理员 → 导出确认 → P2P 启动）
// ---------------------------------------------------------------------------

#[test]
fn purge_preview_and_execute_guards() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    kernel
        .declare_collection("plugin:app", "notes", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put("plugin:app", "notes", "n1", json!({"v": 1}), CollectionConfig::default())
        .unwrap();
    kernel
        .doc_put("plugin:app", "notes", "n2", json!({"v": 2}), CollectionConfig::default())
        .unwrap();

    let before_ts = system_now_ms() + 3_600_000;
    // 预览：两篇受影响；p2p 未启动 → replica 为 None；管理员标记 true
    let preview = kernel.preview_purge(&org_id, before_ts).unwrap();
    assert_eq!(preview.domain, "plugin:app");
    assert_eq!(preview.preview.affected_docs, 2);
    assert!(preview.is_current_user_admin);
    assert!(preview.replica.is_none());
    // 预览不写：文档仍在
    assert!(kernel.doc_get("plugin:app", "notes", "n1").unwrap().is_some());

    // 未确认导出 → 拒绝（管理员校验在前但已满足）
    let err = kernel.execute_purge(&org_id, before_ts, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Export backup first: confirmExported must be true before purging"
    );
    // p2p 未启动 → 拒绝
    let err = kernel.execute_purge(&org_id, before_ts, true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "P2P network is not started; cannot verify replica sufficiency, purge refused"
    );

    // p2p 启动但副本不足（1/3）→ 拒绝
    kernel.start_p2p().unwrap();
    let err = kernel.execute_purge(&org_id, before_ts, true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Replica insufficient (1/3): purging local copies now may lose organization data. \
         Wait for replicas to replenish or add disk space instead."
    );
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 数据治理：usage / cleanup / export
// ---------------------------------------------------------------------------

#[test]
fn usage_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel
        .doc_put("chat", "misc", "k1", json!({"v": 1}), CollectionConfig::default())
        .unwrap();

    let usage = kernel.get_usage().unwrap();
    assert!(usage.total_keys >= 1);
    assert!(usage.disk.is_some(), "数据目录给定 → 含磁盘信息");

    let cleanup = kernel.run_cleanup_now().unwrap();
    assert_eq!(cleanup.tombstones, 0, "无过期墓碑");

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// p2p 起停：事件流、状态查询、运行期 doc 广播不炸
// ---------------------------------------------------------------------------

#[test]
fn p2p_start_stop_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let mut events = kernel.subscribe_p2p_events();
    let peer_id = kernel.start_p2p().unwrap();
    assert!(!peer_id.is_empty());
    assert!(kernel.p2p_running());

    // Started 事件（单独 runtime 接收；kernel 方法本身同步）
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(15), events.recv()).await })
        .expect("Started event within 15s")
        .expect("event channel open");
    match started {
        P2pEvent::Started { peer_id: started_peer, listen_addresses } => {
            assert_eq!(started_peer, peer_id);
            assert!(!listen_addresses.is_empty());
        }
        other => panic!("expected Started event, got {other:?}"),
    }

    // 状态查询
    let status = kernel.p2p_status().unwrap().expect("started");
    assert!(status.started);
    assert_eq!(status.peer_id.as_deref(), Some(peer_id.as_str()));

    // 幂等 start
    assert_eq!(kernel.start_p2p().unwrap(), peer_id);

    // p2p 运行期写文档（广播无订阅者也不影响本地写入）
    kernel
        .declare_collection("chat", "messages", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put("chat", "messages", "p2p-doc", json!({"v": 1}), CollectionConfig::default())
        .unwrap();
    assert!(kernel.doc_get("chat", "messages", "p2p-doc").unwrap().is_some());

    kernel.stop_p2p().unwrap();
    assert!(!kernel.p2p_running());
    assert!(kernel.p2p_status().unwrap().is_none());
    // 幂等 stop
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}


// ---------------------------------------------------------------------------
// 阶段③c 新增门面：签名/域派生/助记词校验、会话版资料更新、组织成员管理、
// 存证查询、p2p 广播、accept_invite 编排
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
    let seed = spark_core::identity::parse_mnemonic(&mnemonic).unwrap().seed;
    let expected = spark_core::identity::derive_domain_identity(&seed, "plugin:chat");
    assert_eq!(derived.domain_id, expected.id());
    assert_eq!(
        derived.public_key,
        base64::engine::general_purpose::STANDARD.encode(expected.public_key())
    );
    assert_eq!(derived.derivation_path, expected.path);
    assert_eq!(
        kernel.derive_domain_identity("   ").unwrap_err().to_string(),
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
    assert!(kernel.update_profile_session(Some(&"长".repeat(25)), None).is_err());

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

#[test]
fn org_member_management() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let member_root = "ab".repeat(32);

    // 未解锁一律 Locked
    assert_eq!(
        kernel
            .org_add_member("org_x", &member_root, None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    init_identity(&mut kernel);
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "成员组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    // 添加成员：role 固定 member
    let view = kernel.org_add_member(&org_id, &member_root, None).unwrap();
    assert_eq!(view.member_count, 2);
    assert_eq!(view.admin_count, 1);

    // 重复添加 = 更新 nodeInfo（成员数不变）
    let node = spark_core::org::OrganizationNodeInfo {
        peer_id: Some("12D3KooWMemberPeerX".to_string()),
        addresses: vec!["/ip4/1.2.3.4/tcp/15002".to_string()],
    };
    let view = kernel
        .org_add_member(&org_id, &member_root, Some(&node))
        .unwrap();
    assert_eq!(view.member_count, 2);
    let m = view
        .members
        .iter()
        .find(|m| m.root_id == member_root)
        .unwrap();
    assert_eq!(
        m.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some("12D3KooWMemberPeerX")
    );

    // 移除成员
    let view = kernel.org_remove_member(&org_id, &member_root).unwrap();
    assert_eq!(view.member_count, 1);
    // 移除唯一 admin（自己）→ 拒绝
    let self_root = kernel.current_root_id().unwrap().unwrap();
    assert_eq!(
        kernel
            .org_remove_member(&org_id, &self_root)
            .unwrap_err()
            .to_string(),
        "Organization must keep at least one admin"
    );
    // 未知组织
    assert_eq!(
        kernel
            .org_add_member("org_nope", &member_root, None)
            .unwrap_err()
            .to_string(),
        "Organization not found"
    );

    // 删除组织
    kernel.org_delete(&org_id).unwrap();
    assert!(kernel.list_orgs().unwrap().is_empty());
    assert_eq!(
        kernel.org_delete(&org_id).unwrap_err().to_string(),
        "Organization not found"
    );

    kernel.shutdown().unwrap();
}

#[test]
fn evidence_facade_queries() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 空链
    assert_eq!(kernel.evidence_head_hash().unwrap(), None);
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 0);
    assert!(kernel.evidence_entry(1).unwrap().is_none());

    // 写入两篇（enable_evidence 集合）
    kernel
        .declare_collection("plugin:app", "notes", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put("plugin:app", "notes", "n1", json!({"v": 1}), CollectionConfig::default())
        .unwrap();
    kernel
        .doc_put("plugin:app", "notes", "n2", json!({"v": 2}), CollectionConfig::default())
        .unwrap();

    let head = kernel.evidence_head_hash().unwrap();
    assert!(head.as_deref().is_some_and(|h| h.len() == 64));
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 2);

    let first = kernel.evidence_entry(1).unwrap().unwrap();
    assert_eq!(first.domain, "plugin:app");
    assert_eq!(first.collection, "notes");
    assert_eq!(first.id, "n1");
    assert_eq!(first.op.as_str(), "put");
    let second = kernel.evidence_entry(2).unwrap().unwrap();
    assert_eq!(second.prev_hash.as_deref(), Some(first.hash.as_str()));
    assert_eq!(head.as_deref(), Some(second.hash.as_str()), "链头 = 末条 hash");

    // 删除 → 第三条 op=delete，链仍完整
    kernel
        .doc_delete("plugin:app", "notes", "n1", CollectionConfig::default())
        .unwrap();
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 3);
    assert_eq!(
        kernel.evidence_entry(3).unwrap().unwrap().op.as_str(),
        "delete"
    );

    kernel.shutdown().unwrap();
}

#[test]
fn p2p_broadcast_requires_started_node() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let body = spark_core::p2p::build_update_body(
        "plugin:app",
        "notes",
        "n1",
        json!({"v": 1}),
        json!({"vv": {}, "ts": 1}),
        None,
    );
    // 未启动 → TS `p2p node not started`
    assert_eq!(
        kernel
            .p2p_broadcast("spark-sync", body.clone())
            .unwrap_err()
            .to_string(),
        "p2p node not started"
    );

    // 启动后零订阅者广播成功（allowPublishToZeroTopicPeers 口径）
    kernel.start_p2p().unwrap();
    kernel.p2p_broadcast("spark-sync", body).unwrap();
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}

#[test]
fn accept_invite_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 未解锁 → Locked
    assert_eq!(
        kernel.accept_invite("whatever").unwrap_err().to_string(),
        "Root identity is locked"
    );

    let (root_id, _) = init_identity(&mut kernel);

    // 坏邀请码 → 解析错误（发生在 p2p 检查之前，对齐 TS 先解码）
    assert!(kernel.accept_invite("not-a-code").is_err());

    // 自邀拒绝（p2p 未启动也先报自邀，对齐 TS 校验顺序）
    let self_code = encode_org_invite(&OrgInvitePayload::new(
        "org_selfinvite1".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id,
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&self_code).unwrap_err().to_string(),
        "不能接受自己发出的邀请码"
    );

    // 合法他人邀请码但 p2p 未启动 → TS 文案
    let other_code = encode_org_invite(&OrgInvitePayload::new(
        "org_otherinvite".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id: "cd".repeat(32),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&other_code).unwrap_err().to_string(),
        "P2P 网络未启动，无法通过邀请码加入"
    );

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// accept_invite 全流程：原始 P2pNode 扮演邀请方（org-pull 响应宿主），
// kernel 作为加入方完成 连接 → claim 捎带 → 拉取 → 落库确认。
// ---------------------------------------------------------------------------

/// 邀请方宿主：serve 受邀组织快照与 pluginDocs（org-pull 直连响应）。
struct InviteAdminHost {
    org_id: String,
    record_value: Value,
    plugin_docs: Vec<Value>,
}

impl spark_core::p2p::P2pHost for InviteAdminHost {
    fn handle_org_pull_list(
        &mut self,
        _payload: Value,
        _remote_peer_id: Option<String>,
    ) -> Result<Value, String> {
        Ok(json!({
            "ok": true,
            "type": "org-pull-list-response",
            "organizations": [{ "orgId": self.org_id }]
        }))
    }

    fn handle_org_pull_org(
        &mut self,
        payload: Value,
        _remote_peer_id: Option<String>,
    ) -> Result<Value, String> {
        let org_id = payload.get("orgId").and_then(Value::as_str).unwrap_or("");
        if org_id != self.org_id {
            return Ok(json!({
                "ok": true,
                "type": "org-pull-org-response",
                "orgId": org_id,
                "status": "removed",
                "reason": "org-not-found"
            }));
        }
        Ok(json!({
            "ok": true,
            "type": "org-pull-org-response",
            "orgId": self.org_id,
            "status": "member",
            "organization": self.record_value,
            "pluginDocs": self.plugin_docs,
        }))
    }
}

#[test]
fn accept_invite_full_flow() {
    // 加入方 kernel（先建身份，管理员记录需要预录其 rootId）
    let joiner_dir = tempfile::tempdir().unwrap();
    let mut joiner = fresh_kernel(joiner_dir.path());
    let (joiner_root, _) = init_identity(&mut joiner);

    // 管理员侧组织记录：创建者 admin + 预录加入方为 member
    let admin_root = "ef".repeat(32);
    let now = system_now_ms();
    let mut admin_storage = spark_core::storage::MemoryStorage::new();
    let record = spark_core::org::OrganizationService::create_organization(
        &mut admin_storage,
        &CreateOrganizationInput {
            name: "邀请组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        },
        &admin_root,
        now,
    )
    .unwrap();
    let org_id = record.org_id.clone();
    let record = spark_core::org::OrganizationService::add_member(
        &mut admin_storage,
        &org_id,
        &joiner_root,
        None,
        &admin_root,
        now,
    )
    .unwrap();
    let record_value = serde_json::to_value(&record).unwrap();

    // 随快照捎带的插件文档（接收方应应用落库）
    let plugin_docs = vec![json!({
        "domain": "plugin:app",
        "collection": "notes",
        "id": "d1",
        "payload": {"text": "hello", "orgId": org_id},
        "meta": {"vv": {"admin-node": 1}, "ts": now, "nodeId": "admin-node"}
    })];

    // 邀请方节点（独立 tokio runtime；P2pNode 句柄可跨 block_on 持有，
    // 事件循环在该 runtime 上存活，拉取完成后显式 stop）
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (admin_peer, admin_addrs, admin_node) = rt.block_on(async {
        let host = InviteAdminHost {
            org_id: org_id.clone(),
            record_value,
            plugin_docs,
        };
        let mut node = spark_core::p2p::P2pNode::start(
            test_p2p_config(),
            spark_core::storage::MemoryStorage::new(),
            Box::new(host),
        )
        .await
        .expect("admin node starts");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let addrs = loop {
            let event = tokio::time::timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                node.next_event(),
            )
            .await
            .expect("Started event")
            .expect("event stream open");
            if let P2pEvent::Started { listen_addresses, .. } = event {
                break listen_addresses;
            }
        };
        let peer = node.peer_id().to_string();
        let dialable: Vec<String> = addrs
            .iter()
            .filter(|a| a.contains("/ip4/"))
            .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
            .collect();
        (peer, dialable, node)
    });
    assert!(!admin_addrs.is_empty(), "管理员节点应有可拨地址");

    // 邀请码：管理员 rootId + 节点信息
    let code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "邀请组织".to_string(),
        OrgInviteInviter {
            root_id: admin_root.clone(),
            peer_id: Some(admin_peer),
            addresses: admin_addrs,
        },
        system_now_ms(),
    ));

    // 加入方启动 p2p 并接受邀请：连接 → claim 捎带 → 拉取 → 落库确认
    joiner.start_p2p().unwrap();
    let acceptance = joiner.accept_invite(&code).unwrap();
    assert_eq!(acceptance.org_id, org_id);
    assert_eq!(acceptance.org_name, "邀请组织");
    assert_eq!(acceptance.member_count, 2);

    // 组织记录落库：当前用户为 member 角色
    let mine = joiner.list_orgs().unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].record.org_id, org_id);
    assert!(!mine[0].is_current_user_admin);
    assert_eq!(mine[0].member_count, 2);

    // pluginDocs 已应用
    let doc = joiner.doc_get("plugin:app", "notes", "d1").unwrap();
    assert_eq!(doc, Some(json!({"text": "hello", "orgId": org_id})));

    joiner.shutdown().unwrap();
    rt.block_on(admin_node.stop());
}

// ---------------------------------------------------------------------------
// 阶段③c 组织同步编排：双 kernel 互连对跑
// （org-share 推送 / org-pull 响应方 / 反熵对账 / keepalive）
// ---------------------------------------------------------------------------

/// 取 kernel 的可拨地址（0.0.0.0 → 127.0.0.1，仅 ip4）。
fn dialable_addrs(kernel: &Kernel) -> Vec<String> {
    kernel
        .p2p_status()
        .unwrap()
        .expect("p2p started")
        .addresses
        .iter()
        .filter(|a| a.contains("/ip4/"))
        .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
        .collect()
}

/// 轮询直到 cond 成立（默认 20s 预算，200ms 间隔）。
fn wait_until(mut cond: impl FnMut() -> bool, budget_ms: u64, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_millis(budget_ms);
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timeout waiting for: {what}");
}

/// org-share 推送编排：A add_member 触发推送 → B（目标 rootId）落库 + A 记账。
#[test]
fn org_share_push_delivers_between_kernels() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织并把 B 预录为成员（带 B 的真实 nodeInfo → 推送可直连送达）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "推送组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        peer_id: Some(kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap()),
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a.org_add_member(&org_id, &root_b, Some(&b_node)).unwrap();

    // B 收到快照落库（org-share 接收应答：target 匹配 + 成员包含 + merge）
    wait_until(
        || kernel_b.list_orgs().map(|l| l.len() == 1).unwrap_or(false),
        20_000,
        "B 收到组织快照",
    );
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b[0].record.org_id, org_id);
    assert_eq!(mine_b[0].member_count, 2);
    assert!(!mine_b[0].is_current_user_admin, "B 为 member 角色");
    // B 侧记录成员集与 A 一致
    let members_a = kernel_a.list_orgs().unwrap();
    assert_eq!(members_a[0].member_count, 2);

    // A 对 B 的 sync-state 已记账（K 副本口径：B everSynced）
    let overview = kernel_a.org_overview(&org_id).unwrap();
    let b_entry = overview
        .members
        .iter()
        .find(|m| m.root_id == root_b)
        .expect("B 在概览中");
    assert!(b_entry.ever_synced, "直连送达后记账生效");
    assert!(b_entry.last_synced_at.is_some());

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// org-pull 响应方接线：双 kernel accept_invite 全流程（邀请方也是 kernel）。
#[test]
fn accept_invite_two_kernels_full() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B（无 nodeInfo——邀请码引导 claim 回填）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "邀请组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    kernel_a.org_add_member(&org_id, &root_b, None).unwrap();

    // 邀请码：A 的 rootId + 真实节点信息
    let code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "邀请组织".to_string(),
        OrgInviteInviter {
            root_id: root_a.clone(),
            peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
            addresses: dialable_addrs(&kernel_a),
        },
        system_now_ms(),
    ));

    // B 接受邀请：connect → pull-list（捎带 claim）→ pull-org → 落库确认
    let acceptance = kernel_b.accept_invite(&code).unwrap();
    assert_eq!(acceptance.org_id, org_id);
    assert_eq!(acceptance.member_count, 2);
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert!(!mine_b[0].is_current_user_admin);

    // A 侧 claim 已回填 B 的 nodeInfo（handle_org_pull_list 的 claim 应用路径）
    let record_a = kernel_a.list_orgs().unwrap();
    let b_member = record_a[0]
        .members
        .iter()
        .find(|m| m.root_id == root_b)
        .expect("B 是成员");
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();
    assert_eq!(
        b_member.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some(b_peer.as_str()),
        "claim 回填 B 的 peerId"
    );

    // A 再加一名成员：触发向已知成员推送 → B 收到更新（成员数 3）
    let root_c = "cd".repeat(32);
    kernel_a.org_add_member(&org_id, &root_c, None).unwrap();
    wait_until(
        || {
            kernel_b
                .list_orgs()
                .map(|l| l[0].member_count == 3)
                .unwrap_or(false)
        },
        20_000,
        "B 收到成员变更推送",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// 反熵对账 + keepalive 注入 + clear_peer_records：
/// B 的 nodeInfo 地址故意写错（推送不可达），B 经 sync_peer_organizations
/// 显式反熵收敛；短间隔 keepalive 验证 tick 编排不炸。
#[test]
fn reconcile_and_keepalive_converge() {
    let keepalive_config = |dir: &Path| KernelConfig {
        data_dir: dir.to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: Some(P2pConfig {
            keepalive_interval: Some(Duration::from_millis(800)),
            ..test_p2p_config()
        }),
    };
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = Kernel::init(keepalive_config(dir_a.path())).unwrap();
    let mut kernel_b = Kernel::init(keepalive_config(dir_b.path())).unwrap();
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B：peerId 真实但地址错误（推送不可达，B 只能靠自己回拉）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "反熵组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();
    let b_node_broken = spark_core::org::OrganizationNodeInfo {
        peer_id: Some(b_peer.clone()),
        addresses: vec!["/ip4/127.0.0.1/tcp/1".to_string()],
    };
    kernel_a
        .org_add_member(&org_id, &root_b, Some(&b_node_broken))
        .unwrap();
    assert!(kernel_b.list_orgs().unwrap().is_empty(), "推送不可达，B 尚无记录");

    // B 显式反熵：pull-list（memberAuthStatus 凭 peerId 放行）→ B 无本地记录 → 拉取
    let a_node = spark_core::org::OrganizationNodeInfo {
        peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(&kernel_a),
    };
    let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
    assert_eq!(result.pull_checked, 1);
    assert_eq!(result.pull_synced, 1, "B 拉到组织");
    assert_eq!(result.removed, 0);
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert_eq!(mine_b[0].member_count, 2);

    // 版本一致后再对账：skip 分支
    let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
    assert_eq!(result.pull_checked, 1);
    assert_eq!(result.pull_synced, 0);
    assert_eq!(result.skipped, 1, "版本一致跳过");

    // A 侧有向 B 的失败拨号记录 → clear_peer_records 清空
    let cleared = kernel_a.clear_peer_records().unwrap();
    assert!(cleared >= 1, "A 的活跃度记录被清除");
    assert_eq!(kernel_a.clear_peer_records().unwrap(), 0, "已清空");

    // keepalive tick 自然驱动（800ms 间隔）：A 继续拨号 B（错地址失败静默）、
    // B 无候选；观察 B 的 KeepaliveTick 事件证明 tick → worker 链路存活
    let mut events = kernel_b.subscribe_p2p_events();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ticked = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(P2pEvent::KeepaliveTick(_)) => return true,
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false)
    });
    assert!(ticked, "keepalive tick 事件到达（组织保活已触发）");

    // org_keepalive_once：无候选场景幂等不炸
    kernel_b.org_keepalive_once().unwrap();
    kernel_a.org_keepalive_once().unwrap();

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}
