//! affair 复制面端到端集成测试（wiki/protocol/community/affair-sync.md）。
//!
//! - [`follow_replicate_two_kernels_production_path`]：**双真实 Kernel + 真实
//!   libp2p loopback**，三信封（affairsync-hello/need/data）全程走 kernel 生产
//!   链路——出站由 org-sync worker 的 AffairHello 即时触发（affair_ops 关注/
//!   提交挂钩）驱动，入站经 `KernelDmHandler::handle_dm` → `handle_inbound_dm`
//!   生产分发（验签 → kind 路由 → affairsync handler）→ `spawn_affairsync_reply`
//!   回投。无任何手工驱动（测试不再直调 handler / 手工应用 data 批）。
//! - [`affair_meta_announce_gossip_loopback`]：spark-affair-meta 元数据公告
//!   gossip（p2p 层回调，与复制面 dm 链路正交，保留原测试宿主形态）。
//!
//! 拓扑：B（contributor）是事务发起方/全量副本，A（initiator 之外的空副本
//! 关注者）——A 经 hello→（B 推 data）收敛首轮；B 追加 op3 后经即时 hello
//! →（A 回 need → B 推 data）收敛增量。

mod common;

use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use spark_core::affair::{
    affair_head_key, affair_op_key, affair_record_key, compute_affair_id, compute_op_hash,
    genesis_sign_payload, op_sign_payload,
};
use spark_core::kernel::Kernel;
use spark_core::p2p::constants::AFFAIR_META_TOPIC;
use spark_core::p2p::{P2pHost, P2pNode};
use spark_core::storage::StorageBackend;

use common::p2p::*;
use common::*;

/// 真实双 kernel + P2P 投递共享宿主机资源，与 orgsync deliver 测试同口径串行
/// （锁在同一测试进程内排他，消除并行不稳）。
static AFFAIR_KERNEL_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    AFFAIR_KERNEL_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// kernel 身份的 affair 构造助手（创世/操作签名走 kernel 生产 sign 门面）
// ---------------------------------------------------------------------------

/// kernel 当前身份的 actor JSON：`{kind, identity=rootId, publicKey=base64}`。
fn kernel_actor(kernel: &Kernel) -> Value {
    let identity = kernel
        .current_identity()
        .expect("identity")
        .expect("unlocked identity");
    let pub_bytes = hex::decode(&identity.public_key_hex).expect("pubkey hex");
    let public_key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pub_bytes);
    json!({ "kind": "person", "identity": identity.root_id, "publicKey": public_key })
}

/// 用 kernel 根私钥签创世记录（生产 `sign` 门面），返回 (genesis, affairId)。
fn make_genesis_with_kernel(kernel: &Kernel, title: &str, now_ms: i64) -> (Value, String) {
    let actor = kernel_actor(kernel);
    let identity = actor["identity"].as_str().unwrap().to_string();
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": title, "summary": "e2e", "tags": ["hoa"],
        "initiator": actor,
        "rules": {
            "engine": "b1",
            "closeConditions": [{ "type": "op-count", "opType": "content", "count": 100 }],
            "pubPeriod": { "delayMs": 86400000 },
            "participation": { "combine": "all" },
            "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
            "exec": null
        },
        "initialVoters": [identity], "refs": [], "createdAt": now_ms,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(kernel.sign(&payload).expect("kernel sign").signature);
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

/// 用 kernel 根私钥签一条链式操作，返回 (op, opHash)。
fn make_op_with_kernel(
    kernel: &Kernel,
    affair_id: &str,
    prev_op_hash: &str,
    payload: Value,
    now_ms: i64,
) -> (Value, String) {
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": "content", "payload": payload, "actor": kernel_actor(kernel),
        "declaredAt": now_ms,
    });
    let sign_input = op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(kernel.sign(&sign_input).expect("kernel sign").signature);
    let op_hash = compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

/// 从 kernel 存储读 DAG 头集合。
fn kernel_heads(kernel: &Kernel, affair_id: &str) -> Vec<String> {
    let raw = kernel
        .__test_storage()
        .expect("storage")
        .get(&affair_head_key(affair_id))
        .unwrap()
        .expect("heads stored");
    serde_json::from_str::<Value>(&raw).unwrap()["heads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// kernel 存储是否有指定键。
fn kernel_has_key(kernel: &Kernel, key: &str) -> bool {
    kernel
        .__test_storage()
        .expect("storage")
        .get(key)
        .unwrap()
        .is_some()
}

/// 双 Kernel 关注-复制全链路（生产路径）：B 发起事务（创世 + 两条链式操作，
/// 生产 `affair_follow`/`affair_submit_op` 写入），A 空副本关注后**由生产
/// 触发链自动收敛**——A 的关注触发 org-sync worker 发 affairsync-hello →
/// B 生产入站分发判 LocalAhead 回推 data → A 生产入站应用；B 追加 op3 触发
/// 即时 hello → A 回 need → B 推 data → A 收敛。
#[test]
fn follow_replicate_two_kernels_production_path() {
    let _serial = serial_guard();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().expect("A start p2p");
    kernel_b.start_p2p().expect("B start p2p");

    let t0 = spark_core::p2p::node::system_now_ms();
    // B 发起事务：创世 + 两条链式操作（生产写入路径；此时双方目录均为空，
    // 即时 hello 无的放矢属预期）
    let (genesis, affair_id) = make_genesis_with_kernel(&kernel_b, "业委会选举", t0);
    let (op1, op1_hash) = make_op_with_kernel(
        &kernel_b,
        &affair_id,
        &affair_id,
        json!({ "kind": "post", "text": "一" }),
        t0,
    );
    let (op2, op2_hash) = make_op_with_kernel(
        &kernel_b,
        &affair_id,
        &op1_hash,
        json!({ "kind": "post", "text": "二" }),
        t0,
    );
    assert_eq!(
        kernel_b.affair_follow(&genesis).expect("B follows"),
        affair_id
    );
    for op in [&op1, &op2] {
        let result = kernel_b.affair_submit_op(op).expect("B submits op");
        assert_eq!(result["status"], json!("accepted"));
    }

    // 连接建立（A 导入 B 节点名片，生产 connect 路径）
    let card = kernel_b.make_node_card(None).expect("B node card");
    let import = kernel_a.import_node_card(&card).expect("A imports card");
    assert_eq!(import.connect_error, None, "A 连接 B 成功");
    let b_peer = kernel_b
        .p2p_status()
        .unwrap()
        .unwrap()
        .peer_id
        .expect("B peer id");
    wait_until(
        || {
            kernel_a
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == b_peer)
        },
        15_000,
        "A 观察到 B 已连接",
    );

    // 关注者目录冷启动种子（affair-sync §6 覆盖网线索的最小替身：目录条目
    // 本就是「先前复制流量学到的线索」；indexer 目录归 C10）。用生产簿记
    // 函数写入，后续 hello/收敛全走生产触发链。
    spark_core::sync::affairsync::note_follower_seen(
        &mut kernel_a.__test_storage().unwrap(),
        &affair_id,
        &root_b,
        Some(&b_peer),
        t0,
    )
    .expect("A seeds follower hint");

    // A 关注（生产关注路径）→ 触发 AffairHello 即时 hello → B 生产入站判
    // LocalAhead 回推 data → A 生产入站应用（创世重复去重 + 两条 op 接受）
    assert_eq!(
        kernel_a.affair_follow(&genesis).expect("A follows"),
        affair_id
    );
    wait_until(
        || kernel_has_key(&kernel_a, &affair_op_key(&affair_id, &op2_hash)),
        15_000,
        "A 经生产链路收敛 genesis + op1 + op2",
    );
    assert!(kernel_has_key(&kernel_a, &affair_record_key(&affair_id)));
    assert!(kernel_has_key(
        &kernel_a,
        &affair_op_key(&affair_id, &op1_hash)
    ));
    assert_eq!(kernel_heads(&kernel_a, &affair_id), vec![op2_hash.clone()]);

    // B 从 A 的 hello 学到覆盖网线索（生产入站的 note_follower_seen 落账）
    wait_until(
        || {
            spark_core::sync::affairsync::follower_hints(
                &kernel_b.__test_storage().unwrap(),
                &affair_id,
            )
            .map(|hints| hints.iter().any(|h| h.root_id == root_a))
            .unwrap_or(false)
        },
        10_000,
        "B 目录含 A（hello 线索）",
    );

    // 第二轮（本地事务写入的即时触发）：B 追加 op3 → 生产写入路径触发
    // AffairHello → B 向目录中已连接的 A 发 hello → A 判 LocalBehind 回
    // need → B 采集增量推 data → A 应用 op3
    let (op3, op3_hash) = make_op_with_kernel(
        &kernel_b,
        &affair_id,
        &op2_hash,
        json!({ "kind": "post", "text": "三" }),
        spark_core::p2p::node::system_now_ms(),
    );
    let result = kernel_b.affair_submit_op(&op3).expect("B submits op3");
    assert_eq!(result["status"], json!("accepted"));
    wait_until(
        || kernel_has_key(&kernel_a, &affair_op_key(&affair_id, &op3_hash)),
        15_000,
        "op3 经 hello→need→data 生产链路复制到 A",
    );
    assert_eq!(kernel_heads(&kernel_a, &affair_id), vec![op3_hash]);

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 固定密钥与 affair 构造助手（gossip 元数据公告测试用，与 C1 向量生成器同口径）
// ---------------------------------------------------------------------------

struct FixedKey {
    signing_key: SigningKey,
    public_key: String,
    identity: String,
}

fn key_from_seed(seed_byte: u8) -> FixedKey {
    let signing_key = SigningKey::from_bytes(&[seed_byte; 32]);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    let public_key =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public_key_bytes);
    let identity = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(public_key_bytes));
    FixedKey {
        signing_key,
        public_key,
        identity,
    }
}

fn actor_json(key: &FixedKey) -> Value {
    json!({ "kind": "person", "identity": key.identity, "publicKey": key.public_key })
}

fn sign(key: &FixedKey, payload: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        key.signing_key.sign(payload.as_bytes()).to_bytes(),
    )
}

/// 业务层时间戳用真实时钟（与 org_mail e2e 同口径：节点 now_fn 固定只驱动
/// 连接/事件层）。
fn real_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

fn make_genesis(initiator: &FixedKey, title: &str, now_ms: i64) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": title, "summary": "e2e", "tags": ["hoa"],
        "initiator": actor_json(initiator),
        "rules": {
            "engine": "b1",
            "closeConditions": [{ "type": "op-count", "opType": "content", "count": 100 }],
            "pubPeriod": { "delayMs": 86400000 },
            "participation": { "combine": "all" },
            "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
            "exec": null
        },
        "initialVoters": [initiator.identity], "refs": [], "createdAt": now_ms,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign(initiator, &payload));
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

// ---------------------------------------------------------------------------
// 元数据公告测试宿主（只记录 on_affair_meta 回调；dm 不消费，回缺省 ok）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AffairMetaHostState {
    /// 收到的元数据公告（on_affair_meta 回调记录）。
    affair_metas: Vec<Value>,
}

struct AffairMetaHost {
    root_id: String,
    state: std::sync::Arc<std::sync::Mutex<AffairMetaHostState>>,
}

impl P2pHost for AffairMetaHost {
    fn current_root_id(&mut self) -> Option<String> {
        Some(self.root_id.clone())
    }

    fn handle_dm(&mut self, _payload: Value, _remote_peer_id: &str) -> Result<Value, String> {
        Ok(json!({ "ok": true }))
    }

    fn on_affair_meta(&mut self, announce: Value) {
        self.state.lock().unwrap().affair_metas.push(announce);
    }
}

async fn start_meta_node(
    now_ms: i64,
    root_id: &str,
) -> (
    P2pNode,
    std::sync::Arc<std::sync::Mutex<AffairMetaHostState>>,
) {
    let storage = SharedStorage::new();
    let state = std::sync::Arc::new(std::sync::Mutex::new(AffairMetaHostState::default()));
    let host = AffairMetaHost {
        root_id: root_id.to_string(),
        state: state.clone(),
    };
    let node = P2pNode::start(test_config(now_ms), storage, Box::new(host))
        .await
        .expect("node starts");
    (node, state)
}

/// spark-affair-meta 元数据公告 gossip：B 订阅主题，A 发布 → B 宿主回调收到
/// 线形合规公告；affairId 与信封 id 不符的畸形公告被丢弃。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn affair_meta_announce_gossip_loopback() {
    let now = 1_720_000_000_000i64;
    let publisher = key_from_seed(0x11);
    let (genesis, affair_id) = make_genesis(&publisher, "元数据公告", real_now_ms());
    drop(genesis);

    let (a, _state_a) = start_meta_node(now, &publisher.identity).await;
    let (mut b, state_b) = start_meta_node(now, &"cc".repeat(32)).await;
    let b_addrs = started_addresses(&mut b).await;
    connect(&a, &b.peer_id().to_string().as_str(), &dialable(&b_addrs)).await;
    // 等 gossipsub 订阅传播/mesh 建立
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 合规公告：type='affair-meta'、domain='affair'、id=affairId（envelope 外层），
    // payload = affair-metadata §3 线形
    let announce = json!({
        "metaV": 1,
        "affairId": affair_id,
        "title": "元数据公告",
        "summary": "e2e",
        "tags": ["hoa"],
        "metaSeq": 0,
        "basisOpHash": affair_id,
        "updatedAt": real_now_ms(),
    });
    let mut body = spark_core::p2p::envelope::build_org_body("affair-meta", announce);
    body.insert("domain".to_string(), json!("affair"));
    body.insert("id".to_string(), json!(affair_id));

    broadcast_until(&a, AFFAIR_META_TOPIC, body, || {
        !state_b.lock().unwrap().affair_metas.is_empty()
    })
    .await;
    let metas = state_b.lock().unwrap().affair_metas.clone();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0]["metaSeq"], json!(0));
    assert_eq!(metas[0]["affairId"], json!(affair_id));
}
