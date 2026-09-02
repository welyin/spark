//! orgsync（O2a）入站编排 kernel 级集成测试（直调 `handle_inbound_dm`，
//! 手工签名信封，多节点 MemoryStorage 模拟复制组反熵收敛）。
//!
//! 目录形态（N3 拆分，按场景）：
//! - [`sync`]：双节点信封往返收敛 + 声明先行随流量同步；
//! - [`auth`]：资格拒绝（非成员/复制组外）+ 键白名单整批拒收；
//! - [`tombstone`]：GC 等待集合 + 墓碑接力 A→B→C + 删除后重建。
//!
//! 与 pdsync 的差异：orgsync 信封 `from`/`to` 是成员 rootId（非自设备），
//! 各节点用各自身份签名；`remote_peer_id` 为对端设备的 libp2p peerId
//! （dlog seen/wm 键按 (rootId, peerId) 设备粒度）。本地写路径用
//! `put_personal`/手动 org dlog 复刻 VersionedStorage 的记账（测试是
//! MemoryStorage，非中间件句柄），数据布局与生产一致。
//!
//! ## 实现问题备注（需求 vs 实现行为）
//!
//! 任务规格要求「复制组外成员发 hello/need/data → 统一 `rejected` 应答」。
//! 实测 `handle_orgsync_hello` 对复制组外成员采取**静默跳过**（该集合
//! `continue`，整体仍回 `ok:true`），而 `handle_orgsync_need`/`handle_orgsync_data`
//! 对复制组外成员回显式 `rejected`。两者安全语义一致（均零合入、不泄露），
//! 仅 hello 的应答措辞与「统一 rejected」不一致。测试按实际行为断言并在
//! 用例 `orgsync_rejects_non_member_and_outside_replication_group` 中覆盖。

mod auth;
#[path = "../common/mod.rs"]
mod common;
mod deliver;
mod merge;
mod orgq;
mod sync;
mod tombstone;

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use spark_core::kernel::{dm_envelope, handle_inbound_dm};
use spark_core::org::OrganizationService;
use spark_core::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
use spark_core::plugindata::{
    Accounts, CollectionDeclaration, DeclareInput, Space, declare, org_data_prefix, org_decl_key,
};
use spark_core::storage::{MemoryStorage, ScanOptions, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::orgsync::{
    build_orgsync_data_batch, build_orgsync_hello, build_orgsync_need, collect_org_collection_vv,
    collect_org_collections, collect_org_incremental, collect_org_tombstones_after,
    org_dlog_current_seq, org_dlog_entries_after,
};
use spark_core::sync::{get_personal_meta, is_tombstone, put_personal};

const ORG_ID: &str = "org_0000000000000001";
const NAME: &str = "ai-chat:finance";
const VERSION: &str = "1.0.0";
const NOW: i64 = 1_720_000_000_000;

/// 自身份：rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致）。
fn self_identity(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

fn member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role,
        joined_at: 1000,
        added_by: "creator".to_string(),
        node_info: None,
        nickname: None,
        avatar: None,
        signature: None,
        gender: None,
        region: None,
        use_personal_identity: None,
        access_key: None,
        extra: Default::default(),
    }
}

/// 构造并保存一个组织记录（成员/数据账号可控）。
fn save_org(
    storage: &mut MemoryStorage,
    org_id: &str,
    members: Vec<(&str, OrganizationRole)>,
    data_accounts: &[&str],
) {
    let record = OrganizationRecord {
        org_id: org_id.to_string(),
        name: "test-org".to_string(),
        description: String::new(),
        avatar: String::new(),
        base_plugin_domain: None,
        created_at: 1000,
        created_by: "creator".to_string(),
        updated_at: 1000,
        members: members
            .into_iter()
            .map(|(rid, role)| member(rid, role))
            .collect(),
        sync: None,
        gateways: vec![],
        data_accounts: data_accounts.iter().map(|r| r.to_string()).collect(),
        org_address: None,
        is_public: false,
        extra: Default::default(),
    };
    OrganizationService::save_record(storage, &record).unwrap();
}

/// 声明一个 org scope 集合并把声明记录写 pmeta（复刻 VersionedStorage：
/// `org:coll:` 是受管系统集合，本测试用 put_personal 记账）。
fn declare_org_collection(
    storage: &mut MemoryStorage,
    node_id: &str,
    org_id: &str,
    name: &str,
    version: &str,
    accounts: Accounts,
    declared_by: &str,
    now: i64,
) -> CollectionDeclaration {
    let decl = declare(
        storage,
        "ai-chat",
        DeclareInput {
            name: name.to_string(),
            version: Some(version.to_string()),
            space: Some(Space::Org),
            accounts: Some(accounts),
            scope: Some(spark_core::plugindata::Scope::Sync),
            declared_by: Some(declared_by.to_string()),
            ..Default::default()
        },
        now,
        Some(org_id),
    )
    .unwrap();
    // 复刻中间件记账：声明记录写 pmeta（否则 collect_org_incremental 不携带）
    let decl_key = org_decl_key(org_id, name, version);
    put_personal(
        storage,
        node_id,
        &decl_key,
        &serde_json::to_string(&decl).unwrap(),
        now,
    )
    .unwrap();
    decl
}

/// A 侧本地写 orgd 数据（put_personal 复刻中间件本地写记账：vv bump + pmeta）。
fn write_org_data(
    storage: &mut MemoryStorage,
    node_id: &str,
    org_id: &str,
    name: &str,
    version: &str,
    key: &str,
    value: &str,
    now: i64,
) -> DocMeta {
    put_personal(
        storage,
        node_id,
        &format!("{}{key}", org_data_prefix(org_id, name, version)),
        value,
        now,
    )
    .unwrap()
}

/// A 侧本地删 orgd 数据（复刻中间件墓碑：per-node 序号 bump + tombstone
/// pmeta + org 域 dlog，同一 batch——即 `org_tombstone_local` 原语）。
fn delete_org_data(
    storage: &mut MemoryStorage,
    node_id: &str,
    org_id: &str,
    name: &str,
    version: &str,
    key: &str,
    now: i64,
) -> (u64, DocMeta) {
    let record_key = format!("{}{key}", org_data_prefix(org_id, name, version));
    let meta = spark_core::sync::orgsync::org_tombstone_local(
        storage,
        node_id,
        org_id,
        name,
        version,
        &record_key,
        now,
    )
    .unwrap();
    let seq = org_dlog_current_seq(storage, org_id, name, version).unwrap();
    (seq, meta)
}

/// 构造并投递一个 orgsync 信封（from/to 为成员 rootId），返回入站结果。
fn deliver_orgsync(
    storage: &mut MemoryStorage,
    my_root: &str,
    my_nick: &str,
    from_key: &SigningKey,
    from_root: &str,
    to_root: &str,
    kind: &str,
    body: serde_json::Value,
    remote_peer_id: &str,
    node_id: &str,
) -> spark_core::kernel::InboundDmResult {
    let envelope = dm_envelope::build_envelope(kind, from_root, to_root, NOW, body, from_key);
    handle_inbound_dm(
        storage,
        my_root,
        my_nick,
        envelope,
        remote_peer_id,
        &HashSet::new(),
        NOW,
        node_id,
        None,
    )
    .unwrap()
}

/// 构造 orgsync-hello（以 sender 视角，面向 recipient 成员）。
fn build_hello_for(
    storage: &MemoryStorage,
    org_id: &str,
    recipient_root: &str,
    recipient_peer: &str,
) -> serde_json::Value {
    let collections = collect_org_collections(
        storage,
        org_id,
        &[(NAME.to_string(), VERSION.to_string())],
        recipient_root,
        recipient_peer,
    )
    .unwrap();
    build_orgsync_hello(org_id, collections, &["data".to_string()], "pc")
}

/// 构造一个 dlogAck 显式指定的 orgsync-hello（模拟对端已确认本机删除日志
/// 到某 seq——用于 GC 水位推进；vv 折叠为空即可）。
fn hello_with_dlog_ack(dlog_ack: u64) -> serde_json::Value {
    let mut collections = serde_json::Map::new();
    collections.insert(
        format!("{NAME}@v{VERSION}"),
        json!({ "vv": {}, "dlogAck": dlog_ack }),
    );
    build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc")
}
